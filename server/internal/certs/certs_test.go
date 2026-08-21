package certs

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// **The pin only means something if the key is kept**, so this is the property the
// whole trust model rests on: a second start presents the certificate the first one
// generated. A server that regenerated would hand every returning client a fingerprint
// it did not pin, and a client doing its job would refuse to come back.
func TestASecondStartPresentsTheSameCertificate(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()

	first, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("first LoadOrCreate: %v", err)
	}
	second, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("second LoadOrCreate: %v", err)
	}

	firstPrint, err := Fingerprint(first)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}
	secondPrint, err := Fingerprint(second)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}
	if firstPrint != secondPrint {
		t.Errorf("a restart changed the fingerprint: %s then %s", firstPrint, secondPrint)
	}
}

// The key is the one file here whose mode is load-bearing: whatever can read it can be
// this server. Asserted rather than trusted, because it is inherited from
// world.WriteAtomic's temporary file and a change there would loosen it in silence.
func TestThePrivateKeyIsNotReadableByAnybodyElse(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := LoadOrCreate(dir); err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	info, err := os.Stat(filepath.Join(dir, KeyFileName))
	if err != nil {
		t.Fatalf("Stat: %v", err)
	}
	if mode := info.Mode().Perm(); mode != keyFileMode {
		t.Errorf("%s is mode %04o, want %04o", KeyFileName, mode, keyFileMode)
	}
}

// An ephemeral world keeps nothing, and that has to include the key.
//
// Structural rather than observed: Ephemeral is handed no path and LoadOrCreate refuses
// an unnamed one, so there is no directory an ephemeral server could write a key into
// even by mistake. What is worth asserting is the consequence — a new identity per call,
// which is what the -tls help text warns an operator about rather than a defect.
func TestAnEphemeralCertificateIsNewEveryTime(t *testing.T) {
	t.Parallel()

	first, err := Ephemeral()
	if err != nil {
		t.Fatalf("Ephemeral: %v", err)
	}
	second, err := Ephemeral()
	if err != nil {
		t.Fatalf("second Ephemeral: %v", err)
	}

	firstPrint, err := Fingerprint(first)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}
	secondPrint, err := Fingerprint(second)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}
	if firstPrint == secondPrint {
		t.Error("two ephemeral certificates share a fingerprint, which no two random keys should")
	}
}

// The fingerprint is the SHA-256 of the leaf's DER — the same bytes the client hashes
// off the wire. Computed independently here, because "whatever the function returns"
// would pin nothing: the number has to be the one a Rust client arrives at.
func TestTheFingerprintIsTheDigestOfTheCertificateOnTheWire(t *testing.T) {
	t.Parallel()

	cert, err := Ephemeral()
	if err != nil {
		t.Fatalf("Ephemeral: %v", err)
	}
	got, err := Fingerprint(cert)
	if err != nil {
		t.Fatalf("Fingerprint: %v", err)
	}

	sum := sha256.Sum256(cert.Certificate[0])
	if want := hex.EncodeToString(sum[:]); got != want {
		t.Errorf("Fingerprint = %s, want the DER's digest %s", got, want)
	}
	if len(got) != sha256.Size*2 {
		t.Errorf("the fingerprint is %d characters, want %d hex characters", len(got), sha256.Size*2)
	}
}

// Half a pair is a state this server never writes, so it is refused rather than
// repaired. Generating a fresh pair over it would silently change the fingerprint every
// client pinned — which is the one failure this package exists to prevent, arrived at by
// trying to be helpful.
func TestHalfAPairIsRefusedRatherThanRegenerated(t *testing.T) {
	t.Parallel()

	missing := map[string]string{
		"the certificate": CertFileName,
		"the key":         KeyFileName,
	}

	for name, remove := range missing {
		t.Run(name+" is missing", func(t *testing.T) {
			t.Parallel()

			dir := t.TempDir()
			if _, err := LoadOrCreate(dir); err != nil {
				t.Fatalf("LoadOrCreate: %v", err)
			}
			if err := os.Remove(filepath.Join(dir, remove)); err != nil {
				t.Fatalf("Remove: %v", err)
			}

			if _, err := LoadOrCreate(dir); err == nil {
				t.Fatal("a half-written pair was accepted")
			}
			// And nothing was written over the survivor: the operator can still see what
			// the server had.
			entries, err := os.ReadDir(dir)
			if err != nil {
				t.Fatalf("ReadDir: %v", err)
			}
			if len(entries) != 1 {
				t.Errorf("the refusal left %d files in the directory, want the one survivor", len(entries))
			}
		})
	}
}

// A pair that exists and cannot be parsed is an error and stays one, for the reason a
// corrupt player record is: the alternative silently replaces something a person may
// still want to look at.
func TestAnUnreadablePairIsAnErrorRatherThanAFreshStart(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := LoadOrCreate(dir); err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	certPath := filepath.Join(dir, CertFileName)
	if err := os.WriteFile(certPath, []byte("not a certificate"), 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	if _, err := LoadOrCreate(dir); err == nil {
		t.Fatal("an unparsable certificate was accepted")
	}

	kept, err := os.ReadFile(certPath)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	if string(kept) != "not a certificate" {
		t.Error("the unreadable certificate was written over")
	}
}

func TestLoadOrCreateRefusesAnUnnamedWorldDirectory(t *testing.T) {
	t.Parallel()

	if _, err := LoadOrCreate(""); err == nil {
		t.Fatal("an empty world directory was accepted; the ephemeral case is Ephemeral's")
	}
}

// Nothing here may print key material. A private key in a log is the same disclosure as
// a private key in a repository, and log lines outlive the process that wrote them.
func TestNoPrivateMaterialIsInAnErrorMessage(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := LoadOrCreate(dir); err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	keyPEM, err := os.ReadFile(filepath.Join(dir, KeyFileName))
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, CertFileName), []byte("not a certificate"), 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	_, loadErr := LoadOrCreate(dir)
	if loadErr == nil {
		t.Fatal("the damaged pair was accepted")
	}
	// The whole PEM and its base64 body both, because an error that quoted the file's
	// contents would most likely do it one line at a time.
	body := strings.TrimSpace(string(keyPEM))
	secrets := []string{body}
	if start := strings.Index(body, "\n"); start >= 0 {
		if end := strings.LastIndex(body, "-----END"); end > start {
			secrets = append(secrets, strings.TrimSpace(body[start+1:end]))
		}
	}
	for _, secret := range secrets {
		if secret != "" && strings.Contains(loadErr.Error(), secret) {
			t.Error("an error message carries private key material")
		}
	}
}
