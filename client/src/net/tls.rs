//! The encrypted transport, and the only one this client has.
//!
//! ## Why there is no plaintext path here
//!
//! An identity token is a bearer credential: whatever can read one off the wire can
//! come back as that player. A client that *could* connect in the clear would make
//! that exposure a setting somebody chooses once and never revisits — and a plaintext
//! session looks correct from both ends, so nobody would notice. The server has no
//! flag either. The only configuration nobody can get wrong is the one that does not
//! exist, which is also why the acceptance rule "never present a stored token over an
//! unencrypted connection" needs no code: there is no unencrypted connection to
//! present one over.
//!
//! ## Trust on first use, and the rule that keeps its weak spot empty
//!
//! A Voxelheim server has no domain name and no issuer, so web PKI has nothing to
//! attest and the default verifier has nothing to check. What this client checks
//! instead is a fingerprint it recorded the first time it connected to that address.
//!
//! The known weakness of trust on first use is that first connection, and the tempting
//! thing to say is that it carries nothing worth taking — a client with no identity
//! file presents an empty token and the server mints a new character. **That is only
//! true of a client that has never played there**, and it was written here as though it
//! were true of all of them. Every player carried over from the plaintext transport has
//! an identity file and no pin, so their first connection after the upgrade would have
//! been a first use that accepted any certificate and then handed it the identity. The
//! weak moment and the valuable moment overlapped for exactly the people who had the
//! most to lose. (Found by the review on PR #165, and worth leaving written down: the
//! sentence was plausible, and being plausible is how it survived being wrong.)
//!
//! So the overlap is removed rather than asserted away: [`TlsWire::connect`] refuses a
//! connection that holds an identity for a server it has never pinned. What remains at
//! the weak moment is a client with nothing to present, which is the case the original
//! sentence described.
//!
//! ## A second fact about the same server
//!
//! The pin lives beside the identity file: same directory, same address-sanitising
//! rule, same atomic write, all reused from [`super::session`] rather than copied. A
//! separate file rather than a second field in the identity file, and that is the one
//! deliberate deviation: the identity file is exactly 32 raw bytes, so giving it a
//! format would make every file already on disk the wrong length — which
//! [`super::session::read_identity`] correctly reads as "not a token" and answers by
//! starting a new character. Adding encryption is not a reason to take everybody's
//! character away.
//!
//! ## What a changed fingerprint means, and what happens
//!
//! It is refused, with a message naming the address, the file, and both fingerprints.
//! There is no bypass flag and no prompt: the two things it can mean are "the operator
//! moved the world without `server-key.pem`" and "somebody is standing between you and
//! that server", and no client can tell them apart. Clearing it is deleting the pin
//! file by hand, which is a deliberate act a person takes after asking the operator for
//! the fingerprint the server logs at startup.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::hash::HashAlgorithm;
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};

use super::session::write_atomically;

/// How a pin file is named, beside the identity file it shares a directory with.
const PIN_SUFFIX: &str = ".pin";

/// A SHA-256 digest as lowercase hex.
const FINGERPRINT_CHARS: usize = 64;

/// The name this client puts in SNI and hands the verifier.
///
/// Fixed, and it is not a hostname in the sense TLS usually means: a server is reached
/// at whatever address the player typed, so there is no name to match and the verifier
/// deliberately ignores this one. It is here because the protocol requires *a* name, and
/// a constant is the honest way to say that nothing depends on it. The server's
/// certificate carries `voxelheim` as a SAN so a general-purpose tool sees a well-formed
/// pair.
const SERVER_NAME: &str = "voxelheim";

/// Where this client keeps the fingerprint it pinned for one server: the identity
/// file's own path with `.pin` after it.
///
/// **Derived from the identity path rather than re-derived from the address**, which
/// is what makes "beside the identity file" true rather than merely usually true.
/// `--identity` relocates the file outright — that is how one machine runs two
/// characters against one server, and how a player puts it somewhere writable — and a
/// pin that stayed behind in the default directory would be the one thing the flag did
/// not move. It would also fail exactly where the flag is most needed: a data directory
/// this client cannot write to.
///
/// Two characters against one server therefore keep two pins of the same fingerprint.
/// Redundant and harmless: each is verified independently, and a certificate is a fact
/// about the server rather than about who is playing.
pub fn pin_path(identity: &Path) -> PathBuf {
    let mut name = identity.as_os_str().to_owned();
    name.push(PIN_SUFFIX);
    PathBuf::from(name)
}

/// Reads the pinned fingerprint for a server.
///
/// Three answers, and the middle one is the point: pinned, never seen, or unreadable. A
/// file that exists and holds something that is not a fingerprint is an **error**, never
/// "no pin" — reading it as absent would silently re-pin whatever answered the next
/// connection, which is exactly the substitution the pin exists to catch.
pub fn read_pin(path: &Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "cannot read the pinned certificate in {}: {err}. Refusing to connect: a pin \
                 that cannot be read is not a pin, and connecting anyway is what an attacker \
                 would want.",
                path.display()
            ));
        }
    };

    let pin = text.trim();
    if pin.len() == FINGERPRINT_CHARS && pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Some(pin.to_ascii_lowercase()));
    }
    Err(format!(
        "{} does not hold a certificate fingerprint ({} characters, expected {FINGERPRINT_CHARS} \
         hexadecimal ones). Refusing to connect. Delete the file to pin this server again.",
        path.display(),
        pin.len()
    ))
}

/// Records `fingerprint` as this server's, atomically.
fn write_pin(path: &Path, fingerprint: &str) -> Result<(), String> {
    let mut contents = String::with_capacity(FINGERPRINT_CHARS + 1);
    contents.push_str(fingerprint);
    contents.push('\n');
    write_atomically(path, contents.as_bytes()).map_err(|err| {
        format!(
            "cannot pin the server's certificate in {}: {err}",
            path.display()
        )
    })
}

/// The SHA-256 the server logs, computed the same way on this side.
///
/// Taken from the crypto provider's own cipher suites rather than from a hashing crate,
/// which is what keeps this at three dependencies: rustls exposes each suite's hash
/// through [`rustls::crypto::hash::Hash`], and every TLS 1.3 suite `ring` provides that
/// is built on SHA-256 offers exactly the implementation needed. Hand-rolling the digest
/// was never an option — this file implements no cryptography.
fn fingerprint_of(provider: &CryptoProvider, certificate: &CertificateDer<'_>) -> Option<String> {
    let hash = provider
        .cipher_suites
        .iter()
        .filter_map(|suite| suite.tls13())
        .map(|suite| suite.common.hash_provider)
        .find(|hash| hash.algorithm() == HashAlgorithm::SHA256)?;

    let digest = hash.hash(certificate.as_ref());
    let mut hex = String::with_capacity(FINGERPRINT_CHARS);
    for byte in digest.as_ref() {
        // Infallible against a String; the result is discarded for that reason.
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Some(hex)
}

/// The verifier: it checks a fingerprint and nothing else.
///
/// No chain, no issuer, no name, no expiry — there is nothing to check any of those
/// against, and pretending otherwise would be the kind of validation that reads as
/// security and provides none. What it does check is the one fact that distinguishes
/// this server from anybody else claiming to be it.
///
/// `observed` exists because the answer is needed *after* the handshake: on a first
/// connection there is nothing to compare against, and the fingerprint that was
/// presented is what gets written down — but only once the handshake as a whole has
/// succeeded, so a half-completed connection cannot pin anything.
#[derive(Debug)]
struct PinnedServer {
    expected: Option<String>,
    observed: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let Some(presented) = fingerprint_of(&self.provider, end_entity) else {
            return Err(rustls::Error::General(
                "this build has no SHA-256 to fingerprint the server's certificate with".to_owned(),
            ));
        };

        // Recorded before the comparison, so the caller can name what it saw in the
        // refusal below. A lock poisoned by a panic in another thread is not a reason to
        // accept a certificate, so it fails closed.
        match self.observed.lock() {
            Ok(mut slot) => *slot = Some(presented.clone()),
            Err(_) => {
                return Err(rustls::Error::General(
                    "the certificate verifier's state is poisoned".to_owned(),
                ));
            }
        }

        match &self.expected {
            // First use. Accepted here and pinned by the caller once the handshake has
            // finished — see the module comment for why this moment is the safe one.
            None => Ok(ServerCertVerified::assertion()),
            Some(pinned) if *pinned == presented => Ok(ServerCertVerified::assertion()),
            Some(_) => Err(rustls::Error::General(
                "the server presented a certificate that is not the pinned one".to_owned(),
            )),
        }
    }

    /// TLS 1.2 is not spoken here, so there is no 1.2 signature to verify.
    ///
    /// Reachable only if the `tls12` feature were turned back on and the server's floor
    /// lowered with it. Refused rather than delegated, because a verifier that quietly
    /// grew a second protocol version is how a downgrade becomes possible.
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(
            "this client speaks TLS 1.3 only".to_owned(),
        ))
    }

    /// Delegated to rustls, deliberately: pinning replaces *identity* validation, not
    /// the proof that the peer holds the key it presented. Without this the handshake
    /// would accept a certificate copied off the wire by anyone who watched one.
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// What this connection could say about the server's certificate afterwards.
///
/// The session needs the distinction because it decides whether a stored token may be
/// presented and whether a granted one may be kept — see [`TlsWire::connect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pinning {
    /// A fingerprint was already on file and the server matched it.
    Verified,
    /// A first connection: nothing to compare against, and what was presented is now
    /// on file for the next one.
    PinnedNow,
    /// A first connection whose fingerprint could not be written down. The session is
    /// encrypted, but nothing about this server was remembered.
    Unrecorded,
}

/// Why a connection was not made.
#[derive(Debug)]
pub enum ConnectError {
    /// The socket or the handshake failed for an ordinary reason.
    Failed(String),
    /// The server presented a certificate that is not the pinned one. Its own variant
    /// because it is the one failure a player must not treat as a network glitch.
    Substituted(String),
    /// This client holds an identity for the server but has never verified its
    /// certificate, so presenting that identity would mean handing it to whoever
    /// answered. Its own variant for the same reason: it is a decision, not a glitch.
    Unverified(String),
}

impl ConnectError {
    /// What to show the player.
    pub fn message(self) -> String {
        match self {
            Self::Failed(message) | Self::Substituted(message) | Self::Unverified(message) => {
                message
            }
        }
    }
}

/// One encrypted connection, and the two handles a session needs onto it.
///
/// **The socket may be cloned; the TLS state may not.** A `TcpStream` has two
/// independent directions, which is what let the reader and the writer live on separate
/// threads with a handle each. TLS has one state machine covering both, so the two
/// threads share it behind a mutex instead — and the lock is held only across the
/// record-layer work, never across the socket read that waits for a peer. That ordering
/// is the whole design: without it, a reader parked on a 200 ms poll would block every
/// input frame the player is trying to send.
pub struct TlsWire {
    /// This handle's own socket, used for reading and for nothing else.
    sock: TcpStream,

    /// The one handle every write goes through, and the lock that orders them.
    ///
    /// **A TLS record carries a sequence number, so records have to reach the peer in
    /// the order they were encrypted.** Encrypting under one lock and sending under
    /// none would let two threads interleave — the peer would see record 3 before
    /// record 1 and close the connection — so this lock covers the whole
    /// encrypt-then-send pair, and it is taken *before* the connection's. Every path
    /// takes them in that order and none takes them the other way round.
    send: Arc<Mutex<TcpStream>>,

    conn: Arc<Mutex<ClientConnection>>,

    /// Ciphertext read from the socket that rustls has not taken yet.
    ///
    /// The reader thread's alone — the writer's handle carries an empty one and never
    /// touches it — so it needs no lock. It exists because a socket read and a TLS
    /// record are different units, and the difference is not an edge case: a busy
    /// server fills a read with several records routinely. See [`TlsWire::read`].
    pending: Vec<u8>,
}

impl TlsWire {
    /// Connects, verifies the pin, and pins on a first connection.
    ///
    /// `handshake_timeout` bounds the whole handshake; the caller sets its own read
    /// timeout afterwards for the poll loop.
    pub fn connect(
        sock: TcpStream,
        addr: &str,
        pin_file: Option<&Path>,
        holds_identity: bool,
        handshake_timeout: Duration,
    ) -> Result<(Self, Pinning, Option<String>), ConnectError> {
        let Some(pin_file) = pin_file else {
            return Err(ConnectError::Failed(format!(
                "no file can be named to pin {addr}'s certificate in, so this connection could \
                 not be verified now or remembered for next time. Set VOXELHEIM_IDENTITY or \
                 --identity to choose one."
            )));
        };

        let expected = read_pin(pin_file).map_err(ConnectError::Failed)?;

        // **The upgrade case, and it is the one this whole file exists for.** A player
        // who has been on this server before holds a token for it; if no fingerprint is
        // on file, this client has never verified who answers at that address. Going on
        // would mean accepting whatever certificate arrives and then handing it the
        // identity — which is precisely the theft the encryption is here to prevent,
        // performed by the client itself. Every player carried over from the plaintext
        // transport is in exactly this state on their first connection after the
        // upgrade, so it is the common case rather than an exotic one.
        //
        // Refused rather than downgraded to "connect without the token": that would
        // pin whatever answered and leave the *next* connection presenting the identity
        // to it, which delays the exposure by one session and calls it safety. The
        // remedy is in the message and it is a real one — the pin file is plain hex a
        // person can write, and the fingerprint to write into it is the one the server
        // prints at startup.
        if holds_identity && expected.is_none() {
            return Err(ConnectError::Unverified(format!(
                "refusing to connect to {addr}: this client holds an identity for that server but \
                 has never verified its certificate, and presenting the identity now would hand \
                 it to whoever answered.\n\nAsk whoever runs the server for the fingerprint it \
                 logs when it starts, and write it into {} as one line. Or delete the identity \
                 file beside it to join as a new character, which pins the certificate safely \
                 because a new character has nothing to present.",
                pin_file.display()
            )));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(PinnedServer {
            expected: expected.clone(),
            observed: Mutex::new(None),
            provider: Arc::clone(&provider),
        });

        let config = ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .map_err(|err| ConnectError::Failed(format!("cannot configure TLS: {err}")))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::clone(&verifier) as Arc<dyn ServerCertVerifier>)
            .with_no_client_auth();

        // `dangerous()` names the fact that the default verifier has been replaced, and
        // it is the right word for it in general. Here the replacement is stricter than
        // the default would be able to be: web PKI would have nothing to validate
        // against, so the alternative to pinning is not "safer validation", it is none.

        let name = ServerName::try_from(SERVER_NAME)
            .map_err(|err| ConnectError::Failed(format!("cannot name the server: {err}")))?;
        let mut conn = ClientConnection::new(Arc::new(config), name)
            .map_err(|err| ConnectError::Failed(format!("cannot start TLS with {addr}: {err}")))?;

        let mut sock = sock;
        // A generous timeout for the handshake, replaced by the caller's poll interval
        // once the session is up. Without one, a peer that accepts a connection and then
        // says nothing parks this thread until the OS gives up.
        if let Err(err) = sock.set_read_timeout(Some(handshake_timeout)) {
            return Err(ConnectError::Failed(format!(
                "cannot bound the handshake with {addr}: {err}"
            )));
        }

        if let Err(err) = conn.complete_io(&mut sock) {
            let observed = verifier.observed.lock().ok().and_then(|slot| slot.clone());
            return Err(refusal(
                addr,
                pin_file,
                expected.as_deref(),
                observed.as_deref(),
                &err,
            ));
        }

        let observed = verifier
            .observed
            .lock()
            .map_err(|_| {
                ConnectError::Failed("the certificate verifier's state is poisoned".to_owned())
            })?
            .clone();

        // Pinned only now: a handshake that failed for any other reason must not leave a
        // fingerprint behind for the next connection to compare against.
        let mut complaint = None;
        let pinning = if expected.is_some() {
            Pinning::Verified
        } else {
            match observed.as_deref().map(|f| (f, write_pin(pin_file, f))) {
                Some((_, Ok(()))) => Pinning::PinnedNow,
                Some((_, Err(err))) => {
                    // Not fatal, and the asymmetry is deliberate: this session is
                    // already encrypted, and the caller has been told there is no
                    // identity to lose — a token is only stored beside a pin, so a
                    // connection that could not pin never had one to present. What is
                    // lost is memory, which is worth a warning rather than a refusal.
                    complaint = Some(format!(
                        "{err}. This session is encrypted, but the certificate could not be \
                         remembered — so this is a new character, and the next connection to \
                         {addr} will be one too."
                    ));
                    Pinning::Unrecorded
                }
                // Unreachable: the verifier records what it hashed before it decides, and
                // a handshake that got here decided yes. Answered as unrecorded rather
                // than assumed, because the cost of being wrong is a token stored against
                // a server nobody checked.
                None => Pinning::Unrecorded,
            }
        };

        let send = sock.try_clone().map_err(|err| {
            ConnectError::Failed(format!("cannot open a writer for {addr}: {err}"))
        })?;

        Ok((
            Self {
                sock,
                send: Arc::new(Mutex::new(send)),
                conn: Arc::new(Mutex::new(conn)),
                pending: Vec::new(),
            },
            pinning,
            complaint,
        ))
    }

    /// A second handle for the writer thread: its own socket, the same TLS state.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            sock: self.sock.try_clone()?,
            send: Arc::clone(&self.send),
            conn: Arc::clone(&self.conn),
            // Empty, and it stays empty: only the reader reads.
            pending: Vec::new(),
        })
    }

    /// Bounds how long [`Self::read`] blocks before it reports a timeout.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(timeout)
    }

    /// Ends the connection in both directions, which is how a failed write unblocks the
    /// reader.
    pub fn shutdown(&self) -> io::Result<()> {
        self.sock.shutdown(Shutdown::Both)
    }

    /// Reads decrypted bytes, reporting a timeout exactly as a plain socket does.
    ///
    /// The loop is the record layer showing through: a socket read yields ciphertext,
    /// and a TLS record is not a frame, so one read can produce no plaintext at all and
    /// two can produce one message. **Nothing here blocks with the lock held.** The wait
    /// happens on the socket, outside it; the lock covers only feeding rustls what
    /// arrived and taking out whatever that produced.
    pub fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            {
                let mut conn = self.locked()?;
                let read = match conn.reader().read(out) {
                    Ok(read) => read,
                    // rustls reports "nothing buffered" this way; the socket below is
                    // where waiting belongs.
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => 0,
                    Err(err) => return Err(err),
                };
                if read > 0 {
                    return Ok(read);
                }
            }

            // Only when there is nothing left over from the last read. **What arrives
            // from a socket has no relationship to what a TLS record is**: one read can
            // carry three records and half of a fourth, and `read_tls` deliberately
            // takes at most one record per call. Feeding it a buffer and discarding the
            // remainder desynchronises the stream, and the next record decrypts to
            // nothing — which is exactly the bug this replaced, found by running the
            // real client against the real server rather than by any test either side
            // had. Nothing is ever thrown away now: whatever `read_tls` did not take
            // stays here for the next turn of the loop.
            if self.pending.is_empty() {
                let mut ciphertext = [0u8; CIPHERTEXT_CHUNK];
                let received = self.sock.read(&mut ciphertext)?;
                if received == 0 {
                    // The peer closed. Reported as end-of-stream rather than as an
                    // error, which is what the session loop already knows how to
                    // describe.
                    return Ok(0);
                }
                self.pending.extend_from_slice(&ciphertext[..received]);
            }

            // Locked through the field rather than `self.locked()`, which borrows the
            // whole struct: the buffer and the socket are both needed alongside it.
            let mut conn = self.conn.lock().map_err(|_| poisoned())?;
            let mut cursor = &self.pending[..];
            let taken = conn.read_tls(&mut cursor)?;
            self.pending.drain(..taken);

            conn.process_new_packets()
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            let answer_owed = conn.wants_write();
            drop(conn);

            // A key update or an alert arrives mid-session and needs an answer. The
            // writer thread would eventually send one — the client writes input every
            // tick — but "eventually" is not a protocol guarantee. Sent through the same
            // path a frame takes, and *after* the connection's lock is dropped, because
            // the send lock is taken first everywhere and taking them the other way
            // round here is what a deadlock is made of.
            if answer_owed {
                self.send_records(None)?;
            }

            // `read_tls` answers 0 when its own buffer is full rather than when the
            // stream ends, and `process_new_packets` above is what drains it. Looping
            // is therefore progress, not a spin: the plaintext take at the top of the
            // loop is where that drained data comes out.
            if taken == 0 && self.pending.is_empty() {
                return Ok(0);
            }
        }
    }

    /// Encrypts and sends one payload, flushing it onto the socket before returning.
    ///
    /// The flush is not optional the way a buffered writer's is: rustls holds the record
    /// until somebody drains it, so a frame written and not flushed is a frame the server
    /// never sees.
    pub fn write_all(&mut self, payload: &[u8]) -> io::Result<()> {
        self.send_records(Some(payload))
    }

    /// Encrypts `payload` if there is one, then sends whatever rustls has waiting.
    ///
    /// **The socket write happens with the connection's lock released**, and that is the
    /// whole point of the shape. Holding it across the write would park the reader — which
    /// takes the same lock — behind a socket that is not draining, and a peer that is
    /// itself blocked writing to us would then never be read: both sides stalled, each
    /// waiting for the other to make room. Small frames make it unlikely and not
    /// impossible, and "unlikely" is not what a comment three lines up was claiming.
    ///
    /// What replaces it is the send lock, held across encrypt *and* send so the records
    /// reach the peer in the order they were numbered. Order: send, then connection.
    fn send_records(&self, payload: Option<&[u8]>) -> io::Result<()> {
        let mut socket = self.send.lock().map_err(|_| poisoned())?;

        let mut records = Vec::new();
        {
            let mut conn = self.conn.lock().map_err(|_| poisoned())?;
            if let Some(payload) = payload {
                conn.writer().write_all(payload)?;
            }
            while conn.wants_write() {
                conn.write_tls(&mut records)?;
            }
        }

        if records.is_empty() {
            return Ok(());
        }
        socket.write_all(&records)?;
        socket.flush()
    }

    fn locked(&self) -> io::Result<std::sync::MutexGuard<'_, ClientConnection>> {
        self.conn.lock().map_err(|_| poisoned())
    }
}

impl Read for TlsWire {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        Self::read(self, out)
    }
}

impl Write for TlsWire {
    fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
        self.write_all(payload)?;
        Ok(payload.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A panicking thread left the TLS state half-updated, and there is no way back from
/// that: the record layer's counters are what make a stream a stream.
fn poisoned() -> io::Error {
    io::Error::other("the TLS connection's state is poisoned by a panicking thread")
}

/// How much ciphertext one socket read may take. A TLS record is at most 16 KiB plus its
/// overhead, so this is a whole record and a little room.
const CIPHERTEXT_CHUNK: usize = 20 * 1024;

/// The message a failed handshake becomes.
///
/// A substituted certificate gets its own text and its own variant, because it is the
/// one failure a player must not read as a network glitch and retry through. It names
/// both fingerprints and the file, and it offers no bypass: the fix is to ask the
/// operator for the fingerprint their server logs at startup, and then either recognise
/// it or not.
fn refusal(
    addr: &str,
    pin_file: &Path,
    expected: Option<&str>,
    observed: Option<&str>,
    err: &io::Error,
) -> ConnectError {
    match (expected, observed) {
        (Some(expected), Some(observed)) if expected != observed => {
            ConnectError::Substituted(format!(
                "refusing to connect to {addr}: it presented a different certificate than the one \
             pinned for it.\n  pinned:    {expected}\n  presented: {observed}\n\nThis means either \
             that the server was moved or rebuilt without its key, or that something is standing \
             between you and it — and nothing here can tell those apart. Ask whoever runs the \
             server for the fingerprint it logs when it starts. If it matches what was presented, \
             delete {} and connect again to pin the new one.",
                pin_file.display()
            ))
        }
        _ => ConnectError::Failed(format!(
            "cannot establish an encrypted session with {addr}: {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::UnixTime;
    use std::time::{Duration, SystemTime};

    /// A stand-in for a server's certificate.
    ///
    /// Not a valid X.509 anything, and it does not need to be: the verifier hashes the
    /// DER and compares, and never parses it. A real certificate here would test
    /// `rustls-webpki` rather than this file.
    fn certificate(bytes: &'static [u8]) -> CertificateDer<'static> {
        CertificateDer::from(bytes)
    }

    fn verifier(expected: Option<&str>) -> PinnedServer {
        PinnedServer {
            expected: expected.map(str::to_owned),
            observed: Mutex::new(None),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    fn verify(pinned: &PinnedServer, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let name = ServerName::try_from(SERVER_NAME).expect("the fixed name parses");
        pinned
            .verify_server_cert(cert, &[], &name, &[], UnixTime::now())
            .map(|_| ())
    }

    /// The fingerprint has to be the number the *server* prints, or a player comparing
    /// the two is comparing nothing. Pinned against an independently computed digest
    /// rather than against whatever this function returns.
    #[test]
    fn the_fingerprint_is_the_sha256_of_the_certificate() {
        let provider = rustls::crypto::ring::default_provider();
        let got = fingerprint_of(&provider, &certificate(b"voxelheim"))
            .expect("the ring provider offers a SHA-256 suite");

        assert_eq!(
            got, "9e4f5406e744a2ae653fc46e62f4ce168b59d1b53785d002c73ce3386d35f01b",
            "the client's fingerprint is not SHA-256 of the certificate's DER"
        );
        assert_eq!(got.len(), FINGERPRINT_CHARS);
    }

    /// First use: nothing to compare against, so the certificate is accepted and what it
    /// hashed to is recorded for the caller to write down.
    #[test]
    fn a_first_connection_pins_what_it_was_shown() {
        let pinned = verifier(None);
        verify(&pinned, &certificate(b"first")).expect("a first connection is accepted");

        let observed = pinned.observed.lock().expect("not poisoned").clone();
        let provider = rustls::crypto::ring::default_provider();
        assert_eq!(
            observed,
            fingerprint_of(&provider, &certificate(b"first")),
            "a first connection did not record what it was shown"
        );
    }

    /// The same server on a later connection: accepted, and silently — a pin that
    /// complained about a match would train players to ignore it.
    #[test]
    fn the_pinned_certificate_is_accepted_again() {
        let provider = rustls::crypto::ring::default_provider();
        let cert = certificate(b"same server");
        let pin = fingerprint_of(&provider, &cert).expect("a SHA-256 suite");

        verify(&verifier(Some(&pin)), &cert).expect("the pinned certificate is accepted");
    }

    /// **The whole point.** A different certificate on an address that has one pinned is
    /// refused, and refused inside the handshake rather than reported afterwards — an
    /// accepted handshake is a session, and a session is where the token goes.
    #[test]
    fn a_substituted_certificate_is_refused() {
        let provider = rustls::crypto::ring::default_provider();
        let pin = fingerprint_of(&provider, &certificate(b"the real server")).expect("a suite");

        let pinned = verifier(Some(&pin));
        let err = verify(&pinned, &certificate(b"somebody else"))
            .expect_err("a substituted certificate was accepted");
        assert!(
            format!("{err}").contains("not the pinned one"),
            "the refusal does not say what happened: {err}"
        );

        // And what it saw is still recorded, because the message a player reads names
        // both fingerprints.
        assert!(
            pinned.observed.lock().expect("not poisoned").is_some(),
            "a refused certificate was not recorded, so the refusal cannot name it"
        );
    }

    /// A pin file round-trips, and the file is exactly what a person would edit by hand:
    /// one lowercase hex line. Clearing a pin is deleting it, so it has to be readable.
    #[test]
    fn a_pin_round_trips_through_the_file() {
        let dir = tempdir();
        let path = dir.join("127.0.0.1_7777.pin");
        let fingerprint = "9e4f5406e744a2ae653fc46e62f4ce168b59d1b53785d002c73ce3386d35f01b";

        write_pin(&path, fingerprint).expect("the pin is written");
        assert_eq!(
            read_pin(&path).expect("the pin is readable"),
            Some(fingerprint.to_owned())
        );

        let contents = std::fs::read_to_string(&path).expect("readable");
        assert_eq!(
            contents,
            format!("{fingerprint}\n"),
            "the file is not one plain line"
        );
    }

    /// Whitespace and case are forgiven, because the file is meant to be edited by hand
    /// and a fingerprint copied out of a log arrives however the terminal left it.
    #[test]
    fn a_hand_edited_pin_is_read_as_written() {
        let dir = tempdir();
        let path = dir.join("hand-edited.pin");
        let fingerprint = "9E4F5406E744A2AE653FC46E62F4CE168B59D1B53785D002C73CE3386D35F01B";
        std::fs::write(&path, format!("  {fingerprint}  \n\n")).expect("written");

        assert_eq!(
            read_pin(&path).expect("readable"),
            Some(fingerprint.to_ascii_lowercase())
        );
    }

    /// An address nobody has connected to has no pin, and that is a first connection
    /// rather than a failure.
    #[test]
    fn an_unvisited_server_has_no_pin() {
        let dir = tempdir();
        assert_eq!(read_pin(&dir.join("never-seen.pin")), Ok(None));
    }

    /// **A pin that cannot be read is not a pin.** Reading a damaged file as "no pin"
    /// would re-pin whatever answered the next connection, which is precisely the
    /// substitution the file exists to catch — so every shape of damage is an error.
    #[test]
    fn a_damaged_pin_refuses_rather_than_re_pinning() {
        let dir = tempdir();

        let damage: [(&str, &str); 4] = [
            ("empty", ""),
            ("too short", "9e4f5406"),
            ("not hexadecimal", &"z".repeat(FINGERPRINT_CHARS)),
            ("a whole certificate", "-----BEGIN CERTIFICATE-----"),
        ];

        for (name, contents) in damage {
            let path = dir.join(format!("{name}.pin"));
            std::fs::write(&path, contents).expect("written");
            assert!(
                read_pin(&path).is_err(),
                "a {name} pin file was not refused"
            );
        }
    }

    /// The refusal names the address, the file and both fingerprints, and offers no
    /// bypass — the two things a changed certificate can mean are indistinguishable from
    /// here, so the only honest next step is asking the operator.
    #[test]
    fn the_refusal_tells_a_player_what_to_do() {
        let err = refusal(
            "example:7777",
            Path::new("/data/voxelheim/identity/example_7777.pin"),
            Some("aaaa"),
            Some("bbbb"),
            &io::Error::other("handshake failed"),
        );
        assert!(matches!(err, ConnectError::Substituted(_)));

        let message = err.message();
        for expected in [
            "example:7777",
            "aaaa",
            "bbbb",
            "/data/voxelheim/identity/example_7777.pin",
            "delete",
        ] {
            assert!(
                message.to_lowercase().contains(&expected.to_lowercase()),
                "the refusal never mentions {expected}: {message}"
            );
        }
    }

    /// A handshake that failed for an ordinary reason is an ordinary failure. Reporting
    /// every one as a substituted certificate would make the warning that matters
    /// indistinguishable from a flaky network.
    #[test]
    fn an_ordinary_failure_is_not_reported_as_a_substitution() {
        let err = refusal(
            "example:7777",
            Path::new("/tmp/example.pin"),
            Some("aaaa"),
            Some("aaaa"),
            &io::Error::other("connection reset"),
        );
        assert!(matches!(err, ConnectError::Failed(_)));

        let unreachable = refusal(
            "example:7777",
            Path::new("/tmp/example.pin"),
            None,
            None,
            &io::Error::other("connection refused"),
        );
        assert!(matches!(unreachable, ConnectError::Failed(_)));
    }

    /// The pin sits beside the identity file, under its own name — including when
    /// `--identity` has moved that file somewhere the default derivation would never
    /// have looked. One derivation rather than two that have to agree.
    #[test]
    fn the_pin_follows_the_identity_it_protects() {
        for identity in [
            "/fixture-root/.local/share/voxelheim/identity/127.0.0.1_7777",
            "/somewhere/else/entirely/second-character",
        ] {
            let identity = Path::new(identity);
            let pin = pin_path(identity);

            assert_eq!(
                pin.parent(),
                identity.parent(),
                "the pin left the identity's directory"
            );
            assert_eq!(
                pin.file_name().expect("a name").to_string_lossy(),
                format!(
                    "{}{PIN_SUFFIX}",
                    identity.file_name().expect("a name").to_string_lossy()
                ),
                "the pin does not take the identity file's own name"
            );
        }
    }

    /// **The upgrade case, refused.** A player carried over from the plaintext transport
    /// holds a token and no pin, and going on would accept any certificate and then hand
    /// it the identity. Refused before a single byte is sent — the check runs on the pin
    /// file, ahead of the handshake, so the peer never even learns what was wanted.
    #[test]
    fn an_identity_with_no_pin_refuses_before_the_handshake() {
        let dir = tempdir();
        let pin = dir.join("upgraded.pin");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("its address").to_string();
        let sock = std::net::TcpStream::connect(&addr).expect("connect");

        let err = TlsWire::connect(sock, &addr, Some(&pin), true, Duration::from_millis(200))
            .err()
            .expect("an unpinned server was accepted while holding an identity");

        assert!(
            matches!(err, ConnectError::Unverified(_)),
            "the refusal is not the one a player has to act on: {err:?}"
        );
        let message = err.message();
        for expected in [
            addr.as_str(),
            &pin.display().to_string(),
            "delete the identity",
        ] {
            assert!(
                message.contains(expected),
                "the refusal never mentions {expected}: {message}"
            );
        }

        // Nothing was written, so a later connection is still a clean first use.
        assert!(!pin.exists(), "a refused connection pinned something");
    }

    /// The same server, the same missing pin, and **no** stored identity: allowed to
    /// proceed, because a client with nothing to present has nothing to lose. This is
    /// what makes the rule above a rule about the token rather than about the pin.
    #[test]
    fn a_first_connection_with_no_identity_is_allowed_to_proceed() {
        let dir = tempdir();
        let pin = dir.join("fresh.pin");
        // A listener that accepts and says nothing: the handshake cannot finish, so what
        // is asserted is only *which* way it failed.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("its address").to_string();
        let sock = std::net::TcpStream::connect(&addr).expect("connect");

        let err = TlsWire::connect(sock, &addr, Some(&pin), false, Duration::from_millis(200))
            .err()
            .expect("a silent peer completed a handshake");

        assert!(
            matches!(err, ConnectError::Failed(_)),
            "a first connection with no identity was refused as unverified: {err:?}"
        );
    }

    /// A directory that only this test uses, removed when the process ends.
    ///
    /// Hand-rolled rather than `tempfile`, which would be a fourth dependency for eleven
    /// lines. Unique by process and clock, and the tests here never collide because each
    /// names its own files inside it.
    fn tempdir() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "voxelheim-tls-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("a temporary directory");
        path
    }
}
