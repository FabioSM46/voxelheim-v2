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
//! ## The expectation comes from the list
//!
//! A Voxelheim server has no domain name and no issuer, so web PKI has nothing to
//! attest and the default verifier has nothing to check. What this client checks
//! instead is a fingerprint it was **told to expect before it dialled**: the server
//! list carries a `certificate_sha256` for every server in it, and the account
//! service is where that number comes from.
//!
//! **Trust on first use is gone, and its removal is the point.** This file used to
//! record whatever certificate answered the first connection to an address and compare
//! against that afterwards, which left one connection per server — the first — willing
//! to accept anybody. Leaving that path beside this one would not have been a safety
//! net: two ways to decide who a server is means the weaker one decides whenever the
//! stronger is unavailable, and "the list could not be read" is precisely when an
//! attacker would like the weaker one reached for. So there is no pin file, no reader
//! for one, and no code path anywhere that writes a fingerprint down.
//!
//! ## Why a stored identity is still never presented to an unverified server
//!
//! That rule predates the list and has to survive it. An identity token is a bearer
//! credential; handing one to whoever answers an address is the theft the encryption
//! exists to prevent, performed by the client itself.
//!
//! It now holds **structurally** rather than by a check. [`Expectation`] has two
//! shapes: [`Expectation::Listed`], which carries the fingerprint the list stated, and
//! [`Expectation::Unlisted`], which carries nothing because nothing stated anything.
//! [`super::session::run`] reads and writes the identity file only on the first, so the
//! one path that cannot verify a server is the same path that has no credential to
//! lose. There is no ordering to get right and no flag to forget — the type that omits
//! the fingerprint is the type that omits the identity.
//!
//! [`Expectation::Unlisted`] is reachable only from `--server`, the development path,
//! and it is **never** what an unreadable list falls back to: a list that cannot be
//! read is a screen with a retry on it, and no address at all.
//!
//! ## What a fingerprint that does not match means, and what happens
//!
//! It is refused, with a message naming the address, both fingerprints, and the list as
//! the source of the expectation. There is no bypass flag and no prompt: the two things
//! it can mean are "the operator moved the world without `server-key.pem` and never
//! re-registered it" and "somebody is standing between you and that server", and no
//! client can tell them apart. The fix is on the other side of the list — whoever runs
//! the server reads the fingerprint out of its own startup line and registers *that* —
//! which is a deliberate act taken by somebody who knows why the certificate changed.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::hash::HashAlgorithm;
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};

/// A SHA-256 digest as lowercase hex.
pub(super) const FINGERPRINT_CHARS: usize = 64;

/// The name this client puts in SNI and hands the verifier.
///
/// Fixed, and it is not a hostname in the sense TLS usually means: a server is reached
/// at whatever address the player typed, so there is no name to match and the verifier
/// deliberately ignores this one. It is here because the protocol requires *a* name, and
/// a constant is the honest way to say that nothing depends on it. The server's
/// certificate carries `voxelheim` as a SAN so a general-purpose tool sees a well-formed
/// pair.
const SERVER_NAME: &str = "voxelheim";

/// What this client expects a certificate to be, and where that came from.
///
/// **The three shapes are not three policies.** Two carry a fingerprint somebody stated
/// in advance and differ only in who stated it; the third carries nothing because nobody
/// stated anything, and it exists only because `--server` names an address that is in no
/// list. None is a fallback for another: an unreadable list produces a retry screen,
/// never an [`Expectation::Unlisted`] connection to an address the list would have
/// carried.
///
/// The type is also where "a stored identity is never presented to an unverified
/// server" is enforced. `session::run` opens the identity file on
/// [`Expectation::Listed`] and on nothing else, so the variant that omits the
/// fingerprint is the variant that omits the credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Expectation {
    /// The fingerprint the server list carried for this server, as lowercase hex.
    /// Built only by [`super::servers`], which validates its shape at the boundary.
    Listed(String),
    /// The fingerprint the launch supplied for the **account service**, as lowercase hex.
    ///
    /// A variant of its own rather than a second `Listed`, because the remedy in a
    /// refusal is different and the remedy is the whole value of the message: a listed
    /// server is fixed on the other side of the list, and this one cannot be — it *is*
    /// the list's source, so nothing above it can vouch for it and the number has to
    /// reach a player the way the address does. Built only by
    /// [`super::signin::AccountService`], which validates its shape at the boundary.
    Supplied(String),
    /// `--server`: an address in no list, so there is nothing to check the certificate
    /// against. The session is encrypted and unauthenticated, and it presents no
    /// identity — see the module comment. Development only.
    Unlisted,
}

/// Reads a fingerprint the way this client will compare it: lowercase hex, exactly
/// [`FINGERPRINT_CHARS`] characters, and nothing else accepted.
///
/// Case is folded because the number travels through a log line, a registration and a
/// JSON document before it gets here, and a comparison that failed on case would fail
/// as a substituted certificate — the one refusal a player must not be shown wrongly.
/// Anything that is not a digest is `None`: a list entry this client cannot verify a
/// server against is not a server it can offer, and guessing at a short one would be
/// inventing an expectation.
pub(super) fn parse_fingerprint(raw: &str) -> Option<String> {
    let raw = raw.trim();
    (raw.len() == FINGERPRINT_CHARS && raw.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| raw.to_ascii_lowercase())
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
/// `observed` exists because the answer is needed *after* the handshake: rustls
/// reports a rejected certificate as a handshake failure like any other, and the
/// message a player reads has to name what was presented as well as what was wanted.
/// It is recorded, never stored: nothing in this client writes a fingerprint anywhere.
#[derive(Debug)]
struct PinnedServer {
    expected: Expectation,
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
            Expectation::Listed(stated) | Expectation::Supplied(stated) if *stated == presented => {
                Ok(ServerCertVerified::assertion())
            }
            // **The whole of this file's job.** Refused inside the handshake rather
            // than reported after it: an accepted handshake is a session, and a
            // session is where a credential would go — an identity token on the game
            // wire, an authorization code and a ticket on the account service's.
            Expectation::Listed(_) | Expectation::Supplied(_) => Err(rustls::Error::General(
                "the peer presented a certificate that is not the one that was expected".to_owned(),
            )),
            // Nothing stated what to expect, so there is nothing to compare against and
            // this accepts what it was shown. It is reachable only from `--server`, and
            // it is safe only because that path presents no identity and stores nothing
            // — see [`Expectation`]. It is not a fallback for a list that could not be
            // read, and must never become one.
            Expectation::Unlisted => Ok(ServerCertVerified::assertion()),
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

/// Why a connection was not made.
#[derive(Debug)]
pub(super) enum ConnectError {
    /// The socket or the handshake failed for an ordinary reason.
    Failed(String),
    /// The server presented a certificate that is not the one the list carried. Its own
    /// variant because it is the one failure a player must not treat as a network
    /// glitch and retry through.
    Substituted(String),
}

impl ConnectError {
    /// What to show the player.
    pub(super) fn message(self) -> String {
        match self {
            Self::Failed(message) | Self::Substituted(message) => message,
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
    /// Connects and checks the certificate against what `expected` says to expect.
    ///
    /// `handshake_timeout` bounds the whole handshake; the caller sets its own read
    /// timeout afterwards for the poll loop.
    ///
    /// **Nothing is written down, on any path.** The expectation arrives from the
    /// caller and the fingerprint that was presented leaves only in a refusal message,
    /// which is what "trust on first use is gone" means concretely: there is no file
    /// for a later connection to compare against, so there is no first connection that
    /// decides anything.
    pub(super) fn connect(
        sock: TcpStream,
        addr: &str,
        expected: &Expectation,
        handshake_timeout: Duration,
    ) -> Result<Self, ConnectError> {
        let mut sock = sock;
        let conn = handshake(&mut sock, addr, expected, Some(handshake_timeout))?;

        let send = sock.try_clone().map_err(|err| {
            ConnectError::Failed(format!("cannot open a writer for {addr}: {err}"))
        })?;

        Ok(Self {
            sock,
            send: Arc::new(Mutex::new(send)),
            conn: Arc::new(Mutex::new(conn)),
            pending: Vec::new(),
        })
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
/// both fingerprints and **says where the expectation came from**, which is the one
/// thing this message had to change when the pin file went away: a player who is told
/// "the list says X" knows who to ask, where a player told "a file says X" would go
/// looking for the file and edit it.
///
/// It offers no bypass, and there is deliberately nothing on this side to offer. The
/// remedy is on the server's side of the list — its operator reads the fingerprint out
/// of the startup line and registers that — so the only thing a player can do here is
/// ask them, which is what the text says.
/// Completes one handshake against `expected`, or says why it did not.
///
/// **The one place a TLS client configuration is built in this client**, and it is one
/// place on purpose: the game wire and the account service are different protocols
/// carried over the same guarantee, and two configurations would be two chances to build
/// one that verifies nothing. What differs between the callers is what they wrap the
/// finished connection in, which is below and above this line rather than inside it.
///
/// `handshake_timeout` is `None` when the caller has already bounded the socket — which
/// is what `net/http.rs` does, because a request's read timeout is the same deadline the
/// handshake should be under.
fn handshake(
    sock: &mut TcpStream,
    addr: &str,
    expected: &Expectation,
    handshake_timeout: Option<Duration>,
) -> Result<ClientConnection, ConnectError> {
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
        .map_err(|err| ConnectError::Failed(format!("cannot name the peer: {err}")))?;
    let mut conn = ClientConnection::new(Arc::new(config), name)
        .map_err(|err| ConnectError::Failed(format!("cannot start TLS with {addr}: {err}")))?;

    // A generous timeout for the handshake, replaced by the caller's poll interval
    // once the session is up. Without one, a peer that accepts a connection and then
    // says nothing parks this thread until the OS gives up.
    if let Some(timeout) = handshake_timeout
        && let Err(err) = sock.set_read_timeout(Some(timeout))
    {
        return Err(ConnectError::Failed(format!(
            "cannot bound the handshake with {addr}: {err}"
        )));
    }

    if let Err(err) = conn.complete_io(sock) {
        let observed = verifier.observed.lock().ok().and_then(|slot| slot.clone());
        return Err(refusal(addr, expected, observed.as_deref(), &err));
    }
    Ok(conn)
}

/// One blocking encrypted stream, for the request/response conversations in
/// `net/http.rs`.
///
/// A `StreamOwned` rather than [`TlsWire`] because nothing here needs two handles: an
/// HTTP request is written and then read on one thread, where a session is a reader and a
/// writer parked on different things. The verification is the same verification — same
/// verifier, same comparison, same refusal — which is the property that matters and the
/// reason both go through [`handshake`].
pub(super) type HttpsStream = rustls::StreamOwned<ClientConnection, TcpStream>;

/// Connects `sock` to the account service, refusing anything that is not `expected`.
///
/// The socket arrives already bounded by the caller's timeouts, so the handshake is under
/// the same deadline the request is; see [`handshake`].
pub(super) fn connect_https(
    sock: TcpStream,
    addr: &str,
    expected: &Expectation,
) -> Result<HttpsStream, ConnectError> {
    let mut sock = sock;
    let conn = handshake(&mut sock, addr, expected, None)?;
    Ok(HttpsStream::new(conn, sock))
}

fn refusal(
    addr: &str,
    expected: &Expectation,
    observed: Option<&str>,
    err: &io::Error,
) -> ConnectError {
    match (expected, observed) {
        (Expectation::Listed(listed), Some(observed)) if listed != observed => {
            ConnectError::Substituted(format!(
                "refusing to connect to {addr}: it presented a different certificate than the one \
                 the server list carries for it.\n  the list says: {listed}\n  it presented:   \
                 {observed}\n\nThis means either that the server was moved or rebuilt without its \
                 key and never re-registered, or that something is standing between you and it - \
                 and nothing here can tell those apart. Ask whoever runs the server for the \
                 fingerprint it logs when it starts; if it is the one presented, they need to \
                 register it again before this client will connect."
            ))
        }
        // The account service, whose remedy cannot be "the list will fix it": this hop is
        // what the list arrives over, so the number has to come from whoever runs the
        // service, the same way its address did.
        (Expectation::Supplied(supplied), Some(observed)) if supplied != observed => {
            ConnectError::Substituted(format!(
                "refusing to sign in at {addr}: it presented a different certificate than the one \
                 this client was told to expect.\n  you gave:      {supplied}\n  it presented:  \
                 {observed}\n\nThis means either that whoever runs the account service replaced \
                 its certificate, or that something is standing between you and it - and nothing \
                 here can tell those apart. Ask them for the fingerprint it prints when it starts \
                 (certificate_sha256) and pass that to --account-service-fingerprint. Nothing this \
                 client can read will settle it on its own: this connection is where its trust \
                 begins."
            ))
        }
        _ => ConnectError::Failed(format!(
            "cannot establish an encrypted connection with {addr}: {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::UnixTime;
    use std::time::Duration;

    /// A stand-in for a server's certificate.
    ///
    /// Not a valid X.509 anything, and it does not need to be: the verifier hashes the
    /// DER and compares, and never parses it. A real certificate here would test
    /// `rustls-webpki` rather than this file.
    fn certificate(bytes: &'static [u8]) -> CertificateDer<'static> {
        CertificateDer::from(bytes)
    }

    fn verifier(expected: Expectation) -> PinnedServer {
        PinnedServer {
            expected,
            observed: Mutex::new(None),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    /// What the list would carry for a server presenting `cert`.
    fn listed(cert: &CertificateDer<'_>) -> Expectation {
        let provider = rustls::crypto::ring::default_provider();
        Expectation::Listed(fingerprint_of(&provider, cert).expect("a SHA-256 suite"))
    }

    fn verify(pinned: &PinnedServer, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let name = ServerName::try_from(SERVER_NAME).expect("the fixed name parses");
        pinned
            .verify_server_cert(cert, &[], &name, &[], UnixTime::now())
            .map(|_| ())
    }

    /// The fingerprint has to be the number the *server* prints and the account service
    /// records, or a player comparing them is comparing nothing. Pinned against an
    /// independently computed digest rather than against whatever this function returns.
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

    /// The number the list carried, accepted — and silently. A check that complained
    /// about a match would train players to ignore it.
    #[test]
    fn the_certificate_the_list_named_is_accepted() {
        let cert = certificate(b"the real server");
        verify(&verifier(listed(&cert)), &cert).expect("the listed certificate is accepted");
    }

    /// **The whole point, and the direction that has to fail.** A server presenting
    /// anything other than what the list carried is refused *inside* the handshake
    /// rather than reported after it — an accepted handshake is a session, and a session
    /// is where a credential would go.
    #[test]
    fn a_certificate_the_list_did_not_name_is_refused() {
        let pinned = verifier(listed(&certificate(b"the real server")));
        let err = verify(&pinned, &certificate(b"somebody else"))
            .expect_err("a substituted certificate was accepted");
        assert!(
            format!("{err}").contains("not the one that was expected"),
            "the refusal does not say what happened: {err}"
        );

        // And what it saw is still recorded, because the message a player reads names
        // both fingerprints.
        assert!(
            pinned.observed.lock().expect("not poisoned").is_some(),
            "a refused certificate was not recorded, so the refusal cannot name it"
        );
    }

    /// The development path: nothing stated an expectation, so there is nothing to
    /// compare against and the certificate is accepted. What makes that safe is not
    /// here — it is `session::run`, which opens no identity file on this variant.
    #[test]
    fn an_unlisted_server_has_nothing_to_check_against() {
        let pinned = verifier(Expectation::Unlisted);
        verify(&pinned, &certificate(b"whatever answered"))
            .expect("an unlisted server is not verified at all");
    }

    /// The list's number arrives through a log line, a registration and a JSON document,
    /// so it is read in whatever case and whatever surrounding whitespace it survived —
    /// and anything that is not a digest is refused rather than guessed at. A short or
    /// malformed one is not an expectation, and a list entry carrying one is not a
    /// server this client can offer.
    #[test]
    fn a_fingerprint_is_read_only_when_it_is_one() {
        let digest = "9E4F5406E744A2AE653FC46E62F4CE168B59D1B53785D002C73CE3386D35F01B";
        assert_eq!(
            parse_fingerprint(&format!("  {digest}\n")),
            Some(digest.to_ascii_lowercase())
        );

        for refused in [
            "",
            "9e4f5406",
            &"z".repeat(FINGERPRINT_CHARS),
            &"a".repeat(FINGERPRINT_CHARS + 1),
            "-----BEGIN CERTIFICATE-----",
        ] {
            assert_eq!(parse_fingerprint(refused), None, "{refused}");
        }
    }

    /// The refusal names the address, both fingerprints and **the list** as the source
    /// of the expectation, and offers no bypass: the two things a changed certificate
    /// can mean are indistinguishable from here, so the only honest next step is asking
    /// whoever runs the server.
    #[test]
    fn the_refusal_tells_a_player_what_to_do() {
        let err = refusal(
            "server.example:7777",
            &Expectation::Listed("aaaa".to_owned()),
            Some("bbbb"),
            &io::Error::other("handshake failed"),
        );
        assert!(matches!(err, ConnectError::Substituted(_)));

        let message = err.message();
        for expected in ["server.example:7777", "aaaa", "bbbb", "the server list"] {
            assert!(
                message.contains(expected),
                "the refusal never mentions {expected}: {message}"
            );
        }
        // No bypass is offered, and none exists to offer. The words a player would go
        // looking for are the ones that must not be there.
        for absent in ["delete", "--", "anyway", "ignore"] {
            assert!(
                !message.to_lowercase().contains(absent),
                "the refusal offers a way past itself with {absent:?}: {message}"
            );
        }
    }

    /// **The account service's half of the same comparison** (#131), and it is the same
    /// verifier: a supplied fingerprint is checked exactly as a listed one is, so the
    /// root of the chain is not a second, weaker policy written somewhere else.
    #[test]
    fn a_supplied_fingerprint_is_checked_like_a_listed_one() {
        let cert = certificate(b"the account service");
        let supplied = fingerprint_of(&rustls::crypto::ring::default_provider(), &cert)
            .expect("a SHA-256 suite");

        verify(&verifier(Expectation::Supplied(supplied.clone())), &cert)
            .expect("the supplied certificate is accepted");

        verify(
            &verifier(Expectation::Supplied(supplied)),
            &certificate(b"somebody else"),
        )
        .expect_err("a substituted account service was accepted");
    }

    /// The account service's refusal names the two numbers and the flag to correct — and
    /// it cannot say "the list will fix it", because this hop is what the list arrives
    /// over. Naming a flag here is a remedy rather than a bypass: it is where the right
    /// number goes, and there is no value of it that turns the check off.
    #[test]
    fn the_account_services_refusal_names_the_flag_that_carries_the_number() {
        let err = refusal(
            "accounts.example:7778",
            &Expectation::Supplied("aaaa".to_owned()),
            Some("bbbb"),
            &io::Error::other("handshake failed"),
        );
        assert!(matches!(err, ConnectError::Substituted(_)));

        let message = err.message();
        for expected in [
            "accounts.example:7778",
            "aaaa",
            "bbbb",
            "--account-service-fingerprint",
            "certificate_sha256",
        ] {
            assert!(
                message.contains(expected),
                "the refusal never mentions {expected}: {message}"
            );
        }
        for absent in ["anyway", "ignore", "insecure", "skip"] {
            assert!(
                !message.to_lowercase().contains(absent),
                "the refusal offers a way past itself with {absent:?}: {message}"
            );
        }
    }

    /// A handshake that failed for an ordinary reason is an ordinary failure. Reporting
    /// every one as a substituted certificate would make the warning that matters
    /// indistinguishable from a flaky network.
    #[test]
    fn an_ordinary_failure_is_not_reported_as_a_substitution() {
        let matched = refusal(
            "server.example:7777",
            &Expectation::Listed("aaaa".to_owned()),
            Some("aaaa"),
            &io::Error::other("connection reset"),
        );
        assert!(matches!(matched, ConnectError::Failed(_)));

        let unreachable = refusal(
            "server.example:7777",
            &Expectation::Listed("aaaa".to_owned()),
            None,
            &io::Error::other("connection refused"),
        );
        assert!(matches!(unreachable, ConnectError::Failed(_)));

        // And an unlisted server never produces a substitution, because it never had an
        // expectation to substitute for.
        let unlisted = refusal(
            "server.example:7777",
            &Expectation::Unlisted,
            Some("bbbb"),
            &io::Error::other("connection reset"),
        );
        assert!(matches!(unlisted, ConnectError::Failed(_)));
    }

    /// **Refused before a byte is sent.** The listener accepts the socket and says
    /// nothing back, so the only thing that can end this handshake is the verifier — and
    /// the assertion is that it ends as a substitution rather than as a timeout, which
    /// is what "the peer never learns what was wanted" looks like from this side.
    #[test]
    fn a_server_that_is_not_the_listed_one_is_refused_during_the_handshake() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("its address").to_string();
        let sock = std::net::TcpStream::connect(&addr).expect("connect");

        // A fingerprint no certificate will ever hash to, so whatever answers is wrong.
        let expected = Expectation::Listed("0".repeat(FINGERPRINT_CHARS));
        let err = TlsWire::connect(sock, &addr, &expected, Duration::from_millis(200))
            .err()
            .expect("a silent peer completed a handshake");

        // The peer said nothing at all, so this is a handshake that could not finish
        // rather than a certificate that was rejected — the same answer the old
        // no-identity case gave, and the reason `Substituted` needs a *presented*
        // fingerprint to be reported at all.
        assert!(
            matches!(err, ConnectError::Failed(_)),
            "a peer that presented no certificate was reported as a substitution: {err:?}"
        );
    }
}
