// Package certs is a Voxelheim service's own TLS identity: one self-signed
// certificate, kept under a directory it is given or held in memory for an
// ephemeral world.
//
// **Two services use it, and the directory is the only difference.** The game
// server keeps its pair under -world-dir; the account service keeps its own under
// -auth-dir. Neither may be given the other's, and nothing here enforces that
// because nothing here knows which caller it has — what makes the two identities
// distinct is that they are two directories.
//
// # Why there is no certificate authority here
//
// A Voxelheim service has no domain name and no issuer, so there is nothing for
// web PKI to attest. What a caller checks instead is a fingerprint it was told to
// expect before it dialled — out of band for the account service, out of the
// server list for a game server — which needs a stable public key and nothing
// else. An operator therefore manages no certificate files, renews nothing, and
// installs no ACME client; the whole of the ceremony is that the file below is
// kept.
//
// # What this package will not do
//
// It does not implement cryptography. Key generation, signing and the certificate
// encoding are crypto/ecdsa, crypto/x509 and crypto/tls from the standard library,
// which is also why server/go.mod still has one dependency.
package certs

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"fmt"
	"io/fs"
	"math/big"
	"net"
	"os"
	"path/filepath"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const (
	// CertFileName and KeyFileName are where a persistent identity lives: under the
	// game server's -world-dir beside the world file and the players directory, or
	// under the account service's -auth-dir beside the accounts and the signing key.
	// One pair per directory, so two services sharing neither share no identity.
	CertFileName = "server-cert.pem"
	KeyFileName  = "server-key.pem"

	// keyFileMode is what the private key is written as, and it is the one file in
	// this server whose mode is load-bearing: whatever can read it can be this
	// server. world.WriteAtomic creates its temporary through os.CreateTemp, which
	// is already 0600, and the rename preserves it — this constant is what the test
	// asserts against so that a change to that helper cannot loosen it in silence.
	keyFileMode fs.FileMode = 0o600

	// validity is how long a generated certificate claims to be good for.
	//
	// Ten years, and the number is close to arbitrary on purpose. The client pins a
	// fingerprint and validates nothing else — no chain, no name, no expiry — so
	// this bounds nothing a caller checks. What it does bound is a future in which
	// something else *does* look, and a certificate that expires mid-campaign with
	// no renewal path would be a self-inflicted outage. Rotation is a decision an
	// operator makes by deleting the file.
	validity = 10 * 365 * 24 * time.Hour
)

// LoadOrCreate is the identity kept in dir, generated on first start and read back
// on every one after it.
//
// Both halves are needed for the pin to mean anything: a certificate regenerated on
// every restart would present a new fingerprint each time, and a caller doing its
// job would refuse to reconnect. **A persistent service must keep its key**, which
// is the whole reason this writes a file at all.
//
// A pair that exists and cannot be read is an error and stays one. Regenerating over
// it would hand every pinned caller a refusal they cannot distinguish from an attack,
// on the strength of a permission problem — so the service refuses to start instead,
// which is a message an operator can act on.
func LoadOrCreate(dir string) (tls.Certificate, error) {
	if dir == "" {
		return tls.Certificate{}, errors.New("certs: the directory to keep the certificate in must be named")
	}

	certPath := filepath.Join(dir, CertFileName)
	keyPath := filepath.Join(dir, KeyFileName)

	certPEM, certErr := os.ReadFile(certPath)
	keyPEM, keyErr := os.ReadFile(keyPath)
	switch {
	case certErr == nil && keyErr == nil:
		pair, err := tls.X509KeyPair(certPEM, keyPEM)
		if err != nil {
			return tls.Certificate{}, fmt.Errorf("certs: %s and %s are not a usable pair: %w", certPath, keyPath, err)
		}
		return pair, nil

	case errors.Is(certErr, fs.ErrNotExist) && errors.Is(keyErr, fs.ErrNotExist):
		// The first start. Anything else — one half present, or a read that failed
		// for a reason other than absence — falls through to the refusal below.

	case certErr != nil && !errors.Is(certErr, fs.ErrNotExist):
		return tls.Certificate{}, fmt.Errorf("certs: reading %s: %w", certPath, certErr)
	case keyErr != nil && !errors.Is(keyErr, fs.ErrNotExist):
		return tls.Certificate{}, fmt.Errorf("certs: reading %s: %w", keyPath, keyErr)
	default:
		// Exactly one of the two is missing, which is not a state this server ever
		// writes: the pair is written together. Refused rather than repaired, because
		// generating the missing half is impossible and generating a fresh pair would
		// silently change the fingerprint every client pinned.
		return tls.Certificate{}, fmt.Errorf(
			"certs: %s and %s must both exist or both be absent; delete both to generate a new identity",
			certPath, keyPath)
	}

	if err := os.MkdirAll(dir, 0o755); err != nil {
		return tls.Certificate{}, fmt.Errorf("certs: creating %s: %w", dir, err)
	}
	// Whatever a crash left mid-rename, for the reason every other store under this
	// directory sweeps: this one writes through world.WriteAtomic too. The two names
	// bound it to the pair this package writes — the directory is the operator's, holds
	// files belonging to other packages, and may hold files belonging to nobody here at
	// all (#137).
	world.SweepTemporaries(dir, CertFileName, KeyFileName)

	certPEM, keyPEM, err := generate()
	if err != nil {
		return tls.Certificate{}, err
	}

	// The key first. A crash between the two writes leaves the pair incomplete either
	// way, and the refusal above turns that into a message rather than into a server
	// running under a certificate whose key it does not have.
	if err := world.WriteAtomic(keyPath, keyPEM); err != nil {
		return tls.Certificate{}, fmt.Errorf("certs: writing %s: %w", keyPath, err)
	}
	if err := world.WriteAtomic(certPath, certPEM); err != nil {
		return tls.Certificate{}, fmt.Errorf("certs: writing %s: %w", certPath, err)
	}

	pair, err := tls.X509KeyPair(certPEM, keyPEM)
	if err != nil {
		return tls.Certificate{}, fmt.Errorf("certs: the generated pair does not load: %w", err)
	}
	return pair, nil
}

// Ephemeral is a certificate for a game server with no world directory.
//
// Regenerated per process, and that is the honest consequence of the operator's
// choice rather than a limitation: an ephemeral world keeps nothing, so it cannot
// keep a key either. Every client reconnecting to one will see a fingerprint it did
// not pin and refuse — which is correct, because from the client's side "the server
// changed its key" and "somebody is impersonating the server" are the same
// observation. The flag's help text says so.
func Ephemeral() (tls.Certificate, error) {
	certPEM, keyPEM, err := generate()
	if err != nil {
		return tls.Certificate{}, err
	}
	pair, err := tls.X509KeyPair(certPEM, keyPEM)
	if err != nil {
		return tls.Certificate{}, fmt.Errorf("certs: the generated pair does not load: %w", err)
	}
	return pair, nil
}

// Fingerprint is the SHA-256 of a certificate's DER, lowercase hex.
//
// **The same bytes a caller pins**, which is what makes it comparable at all: a
// caller hashes the leaf certificate exactly as it arrives on the wire, so an
// operator reading this out of a log line, a game server checking the account
// service it fetched a key from, and a player reading it out of a refusal are all
// looking at one number.
//
// Of the certificate and not of the public key. A key fingerprint would survive a
// re-issue with the same key, which sounds like a feature and is not: nothing here
// re-issues anything, so the only thing a changed certificate can mean is a changed
// service.
func Fingerprint(cert tls.Certificate) (string, error) {
	if len(cert.Certificate) == 0 {
		return "", errors.New("certs: the certificate carries no DER to fingerprint")
	}
	sum := sha256.Sum256(cert.Certificate[0])
	return hex.EncodeToString(sum[:]), nil
}

// generate mints one self-signed certificate and its key, both PEM.
//
// ECDSA on P-256 rather than RSA: a smaller key, a faster handshake, and it is what
// both sides' TLS stacks are fastest at. Nothing here chooses a cipher, a curve for
// the key exchange or a protocol version — crypto/tls decides those, and this
// package deliberately has no opinion it could get wrong.
func generate() (certPEM, keyPEM []byte, err error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, nil, fmt.Errorf("certs: generating a key: %w", err)
	}

	// 128 bits from crypto/rand. A serial has to be unique per issuer and this
	// issuer signs exactly one certificate, so the requirement is already met; it is
	// random anyway because a fixed serial makes two servers' certificates look like
	// two issues of one.
	serialMax := new(big.Int).Lsh(big.NewInt(1), 128)
	serial, err := rand.Int(rand.Reader, serialMax)
	if err != nil {
		return nil, nil, fmt.Errorf("certs: generating a serial: %w", err)
	}

	now := time.Now().UTC()
	template := x509.Certificate{
		SerialNumber: serial,
		Subject:      pkix.Name{CommonName: "voxelheim"},
		NotBefore:    now.Add(-time.Hour), // A clock a little behind the server's is not an attack.
		NotAfter:     now.Add(validity),
		KeyUsage:     x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		// Self-signed means self-issued, which means this certificate is its own CA.
		BasicConstraintsValid: true,
		IsCA:                  true,
		// Names nothing an operator has to match. The client pins a fingerprint and
		// does not check a hostname — it cannot, because a server is reached by
		// whatever address the player typed. These are here so that a general-purpose
		// tool poking at the port sees a well-formed certificate rather than one
		// missing a SAN, not because anything in this repository reads them.
		DNSNames:    []string{"voxelheim", "localhost"},
		IPAddresses: []net.IP{net.IPv4(127, 0, 0, 1), net.IPv6loopback},
	}

	der, err := x509.CreateCertificate(rand.Reader, &template, &template, &key.PublicKey, key)
	if err != nil {
		return nil, nil, fmt.Errorf("certs: signing the certificate: %w", err)
	}

	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, nil, fmt.Errorf("certs: encoding the key: %w", err)
	}

	certPEM = pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der})
	keyPEM = pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER})
	return certPEM, keyPEM, nil
}
