package ticket

import (
	"bytes"
	"crypto/ed25519"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// seedOnDisk is the private material this service actually keeps, read straight out of
// the file so that the assertions below are about the real bytes rather than about a
// value a test invented.
//
// **It checks that it found them**, by rebuilding the key and comparing its public half
// against the pair's. A test that searched a log for the wrong 32 bytes would pass while
// proving nothing, which is the one way a secrecy test can be worse than no test.
func seedOnDisk(t *testing.T, dir string, pair *Pair) []byte {
	t.Helper()

	raw, err := os.ReadFile(filepath.Join(dir, SigningKeyFileName))
	if err != nil {
		t.Fatalf("reading the signing key: %v", err)
	}
	if len(raw) != seedRecordSize {
		t.Fatalf("the signing key file is %d bytes, want %d", len(raw), seedRecordSize)
	}
	seed := raw[headerSize() : headerSize()+ed25519.SeedSize]

	rebuilt := ed25519.NewKeyFromSeed(seed).Public().(ed25519.PublicKey)
	if !rebuilt.Equal(pair.Public()) {
		t.Fatal("the bytes this test took for the seed do not rebuild the pair's public key; it is looking at the wrong thing")
	}
	return seed
}

// headerSize keeps the record's own layout out of the assertions above; the guard in
// seedOnDisk is what makes an offset that has moved a failure rather than a false pass.
func headerSize() int { return seedRecordSize - ed25519.SeedSize - 4 }

// **The pair only means something if it is kept**, so this is the property the whole
// design rests on: a service that regenerated on every start would invalidate every
// ticket in flight and every copy a game server had stored, and nobody would find out
// until a player was refused.
func TestASecondStartSignsWithTheSameKey(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	world := midgard(t)
	now := time.Now()

	first, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("first LoadOrCreate: %v", err)
	}
	minted, _, err := first.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}

	second, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("second LoadOrCreate: %v", err)
	}
	if !bytes.Equal(first.Public(), second.Public()) {
		t.Fatalf("a restart changed the public key: %s then %s", first.PublicHex(), second.PublicHex())
	}
	// The assertion that matters is this one rather than the equality above: a ticket
	// minted before the restart is still admitted after it.
	if _, err := Verify(second.Public(), minted[:], world, now); err != nil {
		t.Errorf("a ticket minted before a restart was refused after it: %v", err)
	}
	// And the restarted service still mints tickets the first one's published key
	// admits, which is the same property from the other end.
	after, _, err := second.Mint(anAccount(), world, now)
	if err != nil {
		t.Fatalf("Mint after the restart: %v", err)
	}
	if _, err := Verify(first.Public(), after[:], world, now); err != nil {
		t.Errorf("a ticket minted after a restart was refused by the key published before it: %v", err)
	}
}

// The signing key is the one file here whose mode is load-bearing: whatever can read it
// can mint a ticket for anybody, and there is no revocation to undo that with. Asserted
// rather than trusted, because it is inherited from world.WriteAtomic's temporary file
// and a change there would loosen it in silence.
func TestThePrivateKeyIsNotReadableByAnybodyElse(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if _, err := LoadOrCreate(dir); err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	info, err := os.Stat(filepath.Join(dir, SigningKeyFileName))
	if err != nil {
		t.Fatalf("Stat: %v", err)
	}
	if mode := info.Mode().Perm(); mode != signingKeyFileMode {
		t.Errorf("%s is mode %04o, want %04o", SigningKeyFileName, mode, signingKeyFileMode)
	}
}

// A first start writes the pair and nothing else, and the public half never carries the
// private one — which is what makes the public file safe to hand to anybody who asks.
func TestAFirstStartWritesThePairAndNothingElse(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	pair, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	var names []string
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	if len(names) != 2 {
		t.Errorf("a first start left %v, want exactly the two halves", names)
	}

	seed := seedOnDisk(t, dir, pair)
	published, err := os.ReadFile(filepath.Join(dir, VerifyingKeyFileName))
	if err != nil {
		t.Fatalf("reading the verifying key: %v", err)
	}
	if bytes.Contains(published, seed) {
		t.Error("the public half of the pair carries the private one")
	}
}

// **Half a pair is refused rather than repaired**, and it is refused even in the
// direction that could be repaired: the public half is derivable from the private one,
// and deriving it would mean this service deciding on its own that the survivor is the
// file that is correct. One rule that always says the same thing beats two that depend
// on which file went missing.
func TestHalfAPairIsRefusedRatherThanRegenerated(t *testing.T) {
	t.Parallel()

	for name, remove := range map[string]string{
		"the signing key is missing":   SigningKeyFileName,
		"the verifying key is missing": VerifyingKeyFileName,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := t.TempDir()
			if _, err := LoadOrCreate(dir); err != nil {
				t.Fatalf("LoadOrCreate: %v", err)
			}
			survivor := SigningKeyFileName
			if remove == SigningKeyFileName {
				survivor = VerifyingKeyFileName
			}
			kept, err := os.ReadFile(filepath.Join(dir, survivor))
			if err != nil {
				t.Fatalf("reading the survivor: %v", err)
			}
			if err := os.Remove(filepath.Join(dir, remove)); err != nil {
				t.Fatalf("Remove: %v", err)
			}

			if _, err := LoadOrCreate(dir); err == nil {
				t.Fatal("half a pair was accepted")
			}

			// Nothing was written over the survivor and no new half was invented: the
			// operator can still restore the missing file from a backup.
			entries, err := os.ReadDir(dir)
			if err != nil {
				t.Fatalf("ReadDir: %v", err)
			}
			if len(entries) != 1 {
				t.Errorf("the refusal left %d files in the directory, want the one survivor", len(entries))
			}
			after, err := os.ReadFile(filepath.Join(dir, survivor))
			if err != nil {
				t.Fatalf("reading the survivor again: %v", err)
			}
			if !bytes.Equal(kept, after) {
				t.Error("the surviving half was written over")
			}
		})
	}
}

// **A pair that exists and cannot be read is an error and stays one.** Regenerating over
// it is the tempting thing and the expensive one: every ticket in flight and every copy
// a game server has stored stops working at once, on the strength of a damaged file
// nobody has looked at yet.
func TestAnUnreadablePairIsAnErrorRatherThanAFreshStart(t *testing.T) {
	t.Parallel()

	// Each case damages one file in a way the record discipline is supposed to catch.
	damage := map[string]struct {
		file string
		to   func(original []byte) []byte
	}{
		"a truncated signing key":            {SigningKeyFileName, func(o []byte) []byte { return o[:len(o)-1] }},
		"a padded signing key":               {SigningKeyFileName, func(o []byte) []byte { return append(bytes.Clone(o), 0) }},
		"a signing key of text":              {SigningKeyFileName, func([]byte) []byte { return []byte("not a key") }},
		"a signing key with no magic":        {SigningKeyFileName, withMagic([4]byte{'N', 'O', 'P', 'E'})},
		"a signing key from a newer build":   {SigningKeyFileName, withVersion(KeyStoreVersion + 1)},
		"a signing key with a flipped bit":   {SigningKeyFileName, flipPayloadBit},
		"a truncated verifying key":          {VerifyingKeyFileName, func(o []byte) []byte { return o[:len(o)-1] }},
		"a verifying key with no magic":      {VerifyingKeyFileName, withMagic([4]byte{'N', 'O', 'P', 'E'})},
		"a verifying key from a newer build": {VerifyingKeyFileName, withVersion(KeyStoreVersion + 1)},
		"a verifying key with a flipped bit": {VerifyingKeyFileName, flipPayloadBit},
		// The two records are the same size, which is exactly why the two magics
		// differ: without them a seed would be read as a public key and nothing later
		// in the load would notice.
		"the signing key put where the verifying key goes": {VerifyingKeyFileName, nil},
	}

	for name, how := range damage {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := t.TempDir()
			if _, err := LoadOrCreate(dir); err != nil {
				t.Fatalf("LoadOrCreate: %v", err)
			}
			path := filepath.Join(dir, how.file)

			var damaged []byte
			if how.to == nil {
				var err error
				if damaged, err = os.ReadFile(filepath.Join(dir, SigningKeyFileName)); err != nil {
					t.Fatalf("reading the signing key: %v", err)
				}
			} else {
				original, err := os.ReadFile(path)
				if err != nil {
					t.Fatalf("reading %s: %v", how.file, err)
				}
				damaged = how.to(original)
			}
			if err := os.WriteFile(path, damaged, signingKeyFileMode); err != nil {
				t.Fatalf("WriteFile: %v", err)
			}

			if _, err := LoadOrCreate(dir); err == nil {
				t.Fatal("a damaged pair was accepted")
			}

			// And it was not written over: whatever the operator has is still there to
			// look at, and a backup can still be restored beside it.
			after, err := os.ReadFile(path)
			if err != nil {
				t.Fatalf("reading %s again: %v", how.file, err)
			}
			if !bytes.Equal(after, damaged) {
				t.Error("the unreadable file was written over")
			}
		})
	}
}

// Two halves that are not each other's are refused. Without this check the service
// would sign with one key and publish another — which every gate passes, every log line
// looks right for, and no player can join under.
func TestTwoHalvesThatAreNotAPairAreRefused(t *testing.T) {
	t.Parallel()

	mine, theirs := t.TempDir(), t.TempDir()
	if _, err := LoadOrCreate(mine); err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	if _, err := LoadOrCreate(theirs); err != nil {
		t.Fatalf("LoadOrCreate for the other pair: %v", err)
	}

	stranger, err := os.ReadFile(filepath.Join(theirs, VerifyingKeyFileName))
	if err != nil {
		t.Fatalf("reading the other public key: %v", err)
	}
	if err := os.WriteFile(filepath.Join(mine, VerifyingKeyFileName), stranger, signingKeyFileMode); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	_, err = LoadOrCreate(mine)
	if err == nil {
		t.Fatal("a signing key and somebody else's public key were accepted as a pair")
	}
	if !strings.Contains(err.Error(), "not two halves of one pair") {
		t.Errorf("the refusal is %q, which does not say the two halves disagree", err)
	}
}

func TestLoadOrCreateRefusesAnUnnamedDirectory(t *testing.T) {
	t.Parallel()

	if _, err := LoadOrCreate(""); err == nil {
		t.Fatal("an empty directory was accepted; there is no ephemeral pair, deliberately")
	}
}

// **Nothing here may print key material.** A private key in a log is the same disclosure
// as a private key in a repository, and a log line outlives the process that wrote it.
// Every refusal this package can reach is driven, and the seed is looked for in every
// encoding a leak could take.
func TestNoPrivateMaterialIsInAnErrorMessage(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	pair, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	seed := seedOnDisk(t, dir, pair)
	signingPath := filepath.Join(dir, SigningKeyFileName)
	original, err := os.ReadFile(signingPath)
	if err != nil {
		t.Fatalf("reading the signing key: %v", err)
	}

	// Each of these is a refusal reached with the real seed in the file — including the
	// two where the seed is what the reader is holding when it gives up.
	refusals := map[string]func(t *testing.T) error{
		"a damaged public half": func(t *testing.T) error {
			t.Helper()
			if err := os.WriteFile(filepath.Join(dir, VerifyingKeyFileName), []byte("not a key"), signingKeyFileMode); err != nil {
				t.Fatalf("WriteFile: %v", err)
			}
			_, err := LoadOrCreate(dir)
			return err
		},
		"a signing key whose checksum fails": func(t *testing.T) error {
			t.Helper()
			// The seed is intact and the CRC is not, so the reader has the real bytes
			// in hand at the moment it refuses.
			damaged := bytes.Clone(original)
			damaged[len(damaged)-1] ^= 0xFF
			if err := os.WriteFile(signingPath, damaged, signingKeyFileMode); err != nil {
				t.Fatalf("WriteFile: %v", err)
			}
			_, err := LoadOrCreate(dir)
			return err
		},
		"a signing key from a newer build": func(t *testing.T) error {
			t.Helper()
			if err := os.WriteFile(signingPath, withVersion(KeyStoreVersion+1)(original), signingKeyFileMode); err != nil {
				t.Fatalf("WriteFile: %v", err)
			}
			_, err := LoadOrCreate(dir)
			return err
		},
		"a missing public half": func(t *testing.T) error {
			t.Helper()
			if err := os.WriteFile(signingPath, original, signingKeyFileMode); err != nil {
				t.Fatalf("WriteFile: %v", err)
			}
			if err := os.Remove(filepath.Join(dir, VerifyingKeyFileName)); err != nil {
				t.Fatalf("Remove: %v", err)
			}
			_, err := LoadOrCreate(dir)
			return err
		},
	}

	// Deliberately not parallel: the cases share one directory and take turns damaging
	// it, which is what lets each of them run against the same real seed.
	for name, reach := range refusals {
		refusal := reach(t)
		if refusal == nil {
			t.Fatalf("%s was accepted", name)
		}
		for encoding, leaked := range renderings(seed) {
			if strings.Contains(refusal.Error(), leaked) {
				t.Errorf("the refusal for %s carries the seed as %s: %q", name, encoding, refusal)
			}
		}
	}
}

// The signing key is the one value in this repository whose disclosure is
// unrecoverable, and the four defences are four because each covers a route the others
// do not. The JSON handler is the one a Stringer would not have saved; %#v is the one a
// Stringer never sees.
func TestASigningKeyRedactsItselfWhateverFormatterReachesIt(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	pair, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	seed := seedOnDisk(t, dir, pair)
	key := pair.signing

	var text, jsonOut bytes.Buffer
	slog.New(slog.NewTextHandler(&text, nil)).Info("a key", "key", key)
	slog.New(slog.NewJSONHandler(&jsonOut, nil)).Info("a key", "key", key)
	marshalled, err := json.Marshal(struct {
		Key SigningKey `json:"key"`
	}{key})
	if err != nil {
		t.Fatalf("json.Marshal: %v", err)
	}

	rendered := map[string]string{
		"%v":              fmt.Sprintf("%v", key),
		"%s":              fmt.Sprintf("a key: %s", key),
		"%#v":             fmt.Sprintf("%#v", key),
		"an error":        fmt.Errorf("a key: %v", key).Error(),
		"the text log":    text.String(),
		"the JSON log":    jsonOut.String(),
		"encoding/json":   string(marshalled),
		"a struct holder": fmt.Sprintf("%v", struct{ K SigningKey }{key}),
		"the pair itself": fmt.Sprintf("%v %s %#v", pair, pair, pair),
	}
	for where, got := range rendered {
		for encoding, leaked := range renderings(seed) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the seed as %s: %q", where, encoding, got)
			}
		}
		for encoding, leaked := range renderings(ed25519.NewKeyFromSeed(seed)) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the expanded private key as %s: %q", where, encoding, got)
			}
		}
	}
	if !strings.Contains(fmt.Sprintf("%v", key), redactedSigningKey) {
		t.Error("a signing key does not render as the redaction")
	}
}

// A pair reaching a log line becomes its public key, which is the thing an operator
// wants — so the deliberate disclosure is the default and there is no accident to make.
func TestThePairLogsAsItsPublicKey(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	pair, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}

	var out bytes.Buffer
	slog.New(slog.NewJSONHandler(&out, nil)).Info("the pair", "keys", pair)
	if !strings.Contains(out.String(), pair.PublicHex()) {
		t.Errorf("the log line %q does not carry the public key", out.String())
	}
	if !strings.Contains(out.String(), Algorithm) {
		t.Errorf("the log line %q does not say which algorithm the key is for", out.String())
	}
	if got := len(pair.PublicHex()); got != ed25519.PublicKeySize*2 {
		t.Errorf("the public key renders as %d characters, want %d hex characters", got, ed25519.PublicKeySize*2)
	}
}

// Public hands out a copy. Handing out the pair's own slice would let a caller change
// what this service verifies against by writing into a value it was merely shown.
func TestPublicHandsOutACopy(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	before := pair.PublicHex()

	handed := pair.Public()
	for i := range handed {
		handed[i] ^= 0xFF
	}
	if pair.PublicHex() != before {
		t.Error("writing into the key Public returned changed the pair")
	}
}

// A signing key sits behind HTTP handlers, so two sign-ins minting at once is the
// ordinary case rather than the exotic one. Nothing here is written after LoadOrCreate
// returns, which is what makes that safe — and `go test -race` is what says so.
func TestMintingIsSafeFromManyGoroutinesAtOnce(t *testing.T) {
	t.Parallel()

	pair := newPair(t)
	world := midgard(t)
	now := time.Now()

	const minters = 16
	tickets := make([]Ticket, minters)
	var wg sync.WaitGroup
	for i := range minters {
		wg.Add(1)
		go func() {
			defer wg.Done()
			minted, _, err := pair.Mint(anAccount(), world, now)
			if err != nil {
				t.Errorf("Mint: %v", err)
				return
			}
			tickets[i] = minted
		}()
	}
	wg.Wait()

	for i, minted := range tickets {
		if _, err := Verify(pair.Public(), minted[:], world, now); err != nil {
			t.Errorf("the ticket minted by goroutine %d was refused: %v", i, err)
		}
	}
}

// withMagic replaces a record's magic number.
func withMagic(magic [4]byte) func([]byte) []byte {
	return func(original []byte) []byte {
		damaged := bytes.Clone(original)
		copy(damaged[0:4], magic[:])
		return damaged
	}
}

// withVersion rewrites a record's format version and repairs the checksum, so what the
// reader refuses is the version rather than a CRC that no longer matches.
func withVersion(version uint32) func([]byte) []byte {
	return func(original []byte) []byte {
		damaged := bytes.Clone(original)
		binary.LittleEndian.PutUint32(damaged[4:8], version)
		// Repaired through the same helper the store writes it with, so what the reader
		// refuses is the version rather than a CRC that no longer matches.
		world.PutChecksum(damaged)
		return damaged
	}
}

// flipPayloadBit changes one bit of the key itself and leaves the checksum alone, which
// is the corruption a length check cannot see.
func flipPayloadBit(original []byte) []byte {
	damaged := bytes.Clone(original)
	damaged[headerSize()] ^= 0x01
	return damaged
}
