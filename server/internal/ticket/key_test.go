package ticket

import (
	"bytes"
	"crypto/ed25519"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
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
				// **The offending text is not quoted, and that is the whole point of
				// this test.** A failure here means the refusal carries the signing
				// key; printing it would move the disclosure from a log line nobody
				// meant to write into a CI log this repository publishes. The refusal
				// is named and the encoding is named, which is what a fix needs.
				t.Errorf("the refusal for %s carries the seed as %s; it is not quoted here, "+
					"because a failure that printed it would put the key in a public CI log", name, encoding)
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
	// **Neither loop quotes what it found.** A failure here means the rendering under
	// test contains the signing key, so printing it would publish the key into whatever
	// read the test output — which on this repository is a public CI log. Where it leaked
	// and in which encoding is what a fix needs; the bytes are not.
	for where, got := range rendered {
		for encoding, leaked := range renderings(seed) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the seed as %s (the rendering is deliberately not printed)", where, encoding)
			}
		}
		for encoding, leaked := range renderings(ed25519.NewKeyFromSeed(seed)) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the expanded private key as %s (the rendering is deliberately not printed)", where, encoding)
			}
		}
	}
	if !strings.Contains(fmt.Sprintf("%v", key), redactedSigningKey) {
		t.Error("a signing key does not render as the redaction")
	}
}

// **A [Pair] is redacted by value as well as through a pointer, and until #126 it was
// not.**
//
// String, GoString, LogValue and PublicHex were declared on *Pair, so a Pair *value*
// satisfied neither fmt.Stringer nor slog.LogValuer — and a caller does not have to do
// anything unusual to hold one. `log.Info("keys", "pair", *keys)` is a dereference; a
// struct field of type Pair rather than *Pair is a design choice somebody is entitled to
// make; a `[]Pair` is a slice of values. Every one of those reached slog's default text
// handler, which formats an unrecognised value with `%+v` and walks straight through the
// unexported field into the ed25519 key.
//
// The receivers are what fixes it and this is the test that says so, driven through **real
// handlers rather than a stand-in**: which of the two handlers a value reaches matters
// here, because they fail differently. The text handler prints the key as decimal bytes;
// the JSON handler marshals a struct with no exported fields to `{}` and so leaks
// nothing, while publishing nothing an operator can use either. Only one of those is
// visible in a test that checks for a leak, which is why the disclosure is asserted below
// as well as the secrecy.
//
// [discord.Secret] and [identity.Token] both take value receivers already; Pair was the
// only redacting type in this repository that did not, and the comment above LogValue
// asserted the opposite.
func TestAPairIsRedactedWhenItIsPassedByValue(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	pair, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("LoadOrCreate: %v", err)
	}
	seed := seedOnDisk(t, dir, pair)

	// The dereference is the whole of what this test does differently.
	value := *pair

	var text, jsonOut bytes.Buffer
	slog.New(slog.NewTextHandler(&text, nil)).Info("a pair", "pair", value)
	slog.New(slog.NewJSONHandler(&jsonOut, nil)).Info("a pair", "pair", value)

	rendered := map[string]string{
		"%v on a value":                 fmt.Sprintf("%v", value),
		"%s on a value":                 fmt.Sprintf("a pair: %s", value),
		"%#v on a value":                fmt.Sprintf("%#v", value),
		"an error built from a value":   fmt.Errorf("a pair: %v", value).Error(),
		"a slog text handler":           text.String(),
		"a slog JSON handler":           jsonOut.String(),
		"a struct holding a Pair field": fmt.Sprintf("%v", struct{ P Pair }{value}),
		"a slice of values":             fmt.Sprintf("%v", []Pair{value}),
	}
	// Nothing quoted back, for the reason the test above gives: a failure here means the
	// rendering holds the signing key, and this repository's CI log is public.
	for where, got := range rendered {
		for encoding, leaked := range renderings(seed) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the seed as %s (the rendering is deliberately not printed)", where, encoding)
			}
		}
		for encoding, leaked := range renderings(ed25519.NewKeyFromSeed(seed)) {
			if strings.Contains(got, leaked) {
				t.Errorf("%s carries the expanded private key as %s (the rendering is deliberately not printed)", where, encoding)
			}
		}
	}

	// And the deliberate disclosure survives the dereference. A value that redacted
	// itself into silence would pass every line above while telling an operator nothing,
	// which is the failure the JSON handler makes on its own.
	for where, got := range map[string]string{
		"%v on a value":       fmt.Sprintf("%v", value),
		"a slog text handler": text.String(),
		"a slog JSON handler": jsonOut.String(),
	} {
		if !strings.Contains(got, pair.PublicHex()) {
			// Not quoted either: a rendering that is missing the public key is, in the
			// state this test was written against, a rendering that holds the private one.
			t.Errorf("%s does not carry the public key, which is the one half an operator needs", where)
		}
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

// **Two starts against one directory must not be able to destroy the pair, and until
// #126 they could** (defect 5).
//
// [LoadOrCreate] took no lock, so two callers that both saw an empty directory both
// generated and both wrote — and the four writes are two renames each, which interleave.
// One order leaves the signing half from one pair beside the verifying half of another,
// and the next start refuses with "not two halves of one pair": correct, unrecoverable
// without a backup, and pointing the operator at the one recovery a first start does not
// have. The issue measured 64 damaged directories in 200 rounds.
//
// The assertions are the two halves of what a lock has to buy. Every caller holding the
// same public key is the property a service needs — two of them signing with different
// keys is a fleet where half the tickets fail — and the pair still loading afterwards is
// the property the *next* start needs.
//
// **What this does not buy is two separate processes**, and the limit is written down
// rather than implied: the mutex is this process's. `internal/auth` and `internal/registry`
// serialise their own writes exactly this far and no further, so one `-auth-dir` per
// process is a property of the deployment here as it already is there.
func TestTwoStartsAtOnceLeaveOnePairThatLoads(t *testing.T) {
	t.Parallel()

	// The issue's reproduction: four callers on one directory, enough rounds that the
	// interleaving is not left to a single coin flip.
	const callers, rounds = 4, 200

	for round := range rounds {
		dir := t.TempDir()

		var wg sync.WaitGroup
		release := make(chan struct{})
		pairs := make([]*Pair, callers)
		errs := make([]error, callers)
		for i := range callers {
			wg.Add(1)
			go func() {
				defer wg.Done()
				<-release
				pairs[i], errs[i] = LoadOrCreate(dir)
			}()
		}
		close(release)
		wg.Wait()

		for i, err := range errs {
			if err != nil {
				t.Fatalf("round %d: concurrent start %d answered %v", round, i, err)
			}
		}
		for i := 1; i < callers; i++ {
			if pairs[i].PublicHex() != pairs[0].PublicHex() {
				t.Fatalf("round %d: two concurrent starts are signing with different keys", round)
			}
		}

		// And the next start reads back the pair they left — the assertion the damaged
		// directories failed, one start later, where nobody would connect it to this.
		again, err := LoadOrCreate(dir)
		if err != nil {
			t.Fatalf("round %d: the start after the concurrent ones could not read the pair they left: %v", round, err)
		}
		if again.PublicHex() != pairs[0].PublicHex() {
			t.Fatalf("round %d: the pair on disk is not the one the concurrent starts returned", round)
		}
	}
}

// **`os.MkdirAll(dir, 0o700)` says nothing about a directory that already exists, and
// this is the test that turns the request into a fact** (#126, defect 7).
//
// rename(2) is governed by permission on the directory rather than on the file, so 0600
// on the seed stops anybody else reading it and does nothing at all to stop anybody who
// can write here replacing both halves with a pair of their own — after which this
// service publishes the attacker's public key at an unauthenticated endpoint and every
// game server in the fleet admits whoever they mint for. `mkdir -p` under a default umask
// is 0755, which is to say this is the ordinary deployment rather than an exotic one.
//
// Tightened rather than refused; [secureDir] carries the argument for which, and the
// asymmetry with the key *file* below is deliberate.
func TestAPreCreatedKeyDirectoryIsTightenedBeforeAPairIsWritten(t *testing.T) {
	t.Parallel()

	for name, mode := range map[string]fs.FileMode{
		"the mode mkdir -p leaves under a default umask": 0o755,
		"group writable":                    0o775,
		"writable by anybody at all":        0o777,
		"a group that was meant to read it": 0o750,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := filepath.Join(t.TempDir(), "auth")
			if err := os.Mkdir(dir, 0o700); err != nil {
				t.Fatalf("Mkdir: %v", err)
			}
			// Chmod rather than a mode handed to Mkdir: Mkdir applies the process umask,
			// so the mode a test asks for there is not the mode it gets — which is the
			// same reason t.TempDir() hands back 0777-minus-umask and not 0700.
			if err := os.Chmod(dir, mode); err != nil {
				t.Fatalf("Chmod: %v", err)
			}

			pair, err := LoadOrCreate(dir)
			if err != nil {
				t.Fatalf("LoadOrCreate: %v", err)
			}

			info, err := os.Stat(dir)
			if err != nil {
				t.Fatalf("Stat: %v", err)
			}
			if got := info.Mode().Perm(); got != authDirMode {
				t.Errorf("the key directory is mode %04o after a first start, want %04o; anybody who can write it can replace the pair inside it",
					got, authDirMode)
			}

			// And the pair is a real one that loads, so the tightening happened around
			// the write rather than instead of it.
			again, err := LoadOrCreate(dir)
			if err != nil {
				t.Fatalf("the second start could not read the pair: %v", err)
			}
			if again.PublicHex() != pair.PublicHex() {
				t.Error("the second start read a different pair")
			}
		})
	}

	// A directory loosened *after* the pair was written is tightened on the next start
	// too. The window that matters is every start, not only the first: the pair is
	// replaceable for as long as the directory is writable.
	t.Run("a directory loosened after the first start", func(t *testing.T) {
		t.Parallel()

		dir := filepath.Join(t.TempDir(), "auth")
		if err := os.Mkdir(dir, 0o700); err != nil {
			t.Fatalf("Mkdir: %v", err)
		}
		if _, err := LoadOrCreate(dir); err != nil {
			t.Fatalf("LoadOrCreate: %v", err)
		}
		if err := os.Chmod(dir, 0o777); err != nil {
			t.Fatalf("Chmod: %v", err)
		}
		if _, err := LoadOrCreate(dir); err != nil {
			t.Fatalf("the second start refused: %v", err)
		}
		info, err := os.Stat(dir)
		if err != nil {
			t.Fatalf("Stat: %v", err)
		}
		if got := info.Mode().Perm(); got != authDirMode {
			t.Errorf("a directory loosened after the first start is mode %04o on the next one, want %04o", got, authDirMode)
		}
	})
}

// **A signing key found at a mode anybody else can read is refused, and until #126
// nothing looked** (defect 11).
//
// This package writes the seed at [signingKeyFileMode] and asserts that it did. It never
// asked what mode the file it *found* was at — and the documented recovery for a damaged
// pair is to restore it from a backup, which `cp`, `tar -x` without `-p` and a container
// image layer all land at 0644. The service then starts, mints, publishes, and any local
// account on the machine can read the one value in this repository whose disclosure
// cannot be undone.
//
// **Refused rather than tightened, which is the opposite of what [secureDir] does one
// level up, and the difference is what each one can still buy.** A directory that is too
// open is a risk that can be closed. A key file that is too open is a disclosure that has
// already happened, for however long the file has been sitting there — chmod would fix
// the mode and hide the fact that the pair needs replacing. An operator has to be told.
func TestASigningKeyAnybodyElseCanReadIsRefused(t *testing.T) {
	t.Parallel()

	for name, mode := range map[string]fs.FileMode{
		"world readable, as cp and tar -x without -p leave it": 0o644,
		"readable by a group": 0o640,
		"writable by anybody": 0o666,
		"anybody at all":      0o777,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := t.TempDir()
			pair, err := LoadOrCreate(dir)
			if err != nil {
				t.Fatalf("LoadOrCreate: %v", err)
			}
			seed := seedOnDisk(t, dir, pair)
			signingPath := filepath.Join(dir, SigningKeyFileName)
			before, err := os.ReadFile(signingPath)
			if err != nil {
				t.Fatalf("reading the signing key: %v", err)
			}
			if err := os.Chmod(signingPath, mode); err != nil {
				t.Fatalf("Chmod: %v", err)
			}

			_, err = LoadOrCreate(dir)
			if !errors.Is(err, ErrKeyPermissions) {
				t.Fatalf("a signing key at %04o answered %v, want ErrKeyPermissions", mode, err)
			}
			// The refusal names the file and what to do about it, and carries none of
			// what is inside it. Not quoted on failure, for the reason every other
			// secrecy assertion here is not.
			if !strings.Contains(err.Error(), SigningKeyFileName) {
				t.Errorf("the refusal %q does not name the file that is wrong", err)
			}
			for encoding, leaked := range renderings(seed) {
				if strings.Contains(err.Error(), leaked) {
					t.Errorf("the refusal carries the seed as %s (deliberately not printed)", encoding)
				}
			}

			// And the file was not written over: whatever the operator has is still
			// there, which is the same promise every other refusal in this file makes.
			after, err := os.ReadFile(signingPath)
			if err != nil {
				t.Fatalf("reading the signing key again: %v", err)
			}
			if !bytes.Equal(before, after) {
				t.Error("the refusal wrote over the key file")
			}
		})
	}

	// **The published half is not asked**, and that is not an oversight: it is the value
	// this service serves to anybody who asks for it at GET /v1/ticket-key, so a mode
	// that let somebody read it protects nothing and refusing on it would be a refusal
	// with no threat behind it.
	t.Run("the published half at 0644 is not a refusal", func(t *testing.T) {
		t.Parallel()

		dir := t.TempDir()
		first, err := LoadOrCreate(dir)
		if err != nil {
			t.Fatalf("LoadOrCreate: %v", err)
		}
		if err := os.Chmod(filepath.Join(dir, VerifyingKeyFileName), 0o644); err != nil {
			t.Fatalf("Chmod: %v", err)
		}
		again, err := LoadOrCreate(dir)
		if err != nil {
			t.Fatalf("a world-readable public key was refused: %v", err)
		}
		if again.PublicHex() != first.PublicHex() {
			t.Error("the pair changed")
		}
	})
}

// **A failed second write leaves no orphaned private key** (#126, defect 8).
//
// The pair is written in two renames. When the second one failed, the first was left on
// disk — and from the next start onwards that is half a pair, which this package refuses,
// with a message that used to offer only the recovery a first start cannot have. There is
// no backup of a key that existed for a microsecond, and the correct fix was the one the
// message warned against.
//
// [createPair] is the seam this drives, and it exists because a failure no test can reach
// is how this survived a green suite. The second write is pointed at a path whose parent
// is a **regular file**, so world.WriteAtomic cannot create its temporary there — a
// failure that does not depend on the uid the test runs as, which a permission-based one
// would.
func TestAFailedSecondWriteLeavesNoOrphanedPrivateKey(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	blocker := filepath.Join(dir, "not-a-directory")
	if err := os.WriteFile(blocker, []byte("this is a file, not a directory"), 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}
	signingPath := filepath.Join(dir, SigningKeyFileName)

	_, err := createPair(signingPath, filepath.Join(blocker, VerifyingKeyFileName))
	if err == nil {
		t.Fatal("a pair whose public half could not be written was reported as created")
	}
	if _, statErr := os.Stat(signingPath); !errors.Is(statErr, fs.ErrNotExist) {
		t.Errorf("the private half survived a pair write that failed: Stat answered %v", statErr)
	}
	// The refusal carries the path that could not be written and nothing of the key that
	// was almost created.
	if !strings.Contains(err.Error(), VerifyingKeyFileName) {
		t.Errorf("the refusal %q does not name the write that failed", err)
	}

	// And the directory is a clean first start again, which is the whole point of
	// removing it: an operator who fixes whatever broke the write just restarts.
	if err := os.Remove(blocker); err != nil {
		t.Fatalf("Remove: %v", err)
	}
	pair, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("the start after a failed pair write was refused: %v", err)
	}
	again, err := LoadOrCreate(dir)
	if err != nil {
		t.Fatalf("the pair written after the failure does not load: %v", err)
	}
	if again.PublicHex() != pair.PublicHex() {
		t.Error("the pair written after the failure is not the one that loads")
	}
}

// The half-a-pair refusal names **both** recoveries and says which situation each belongs
// to (#126, defect 8).
//
// It used to offer one: "restore the missing half from a backup", with a warning against
// deleting the other. That is right for a pair that has been in service and useless for
// the state that most often produces this — a first start whose second write failed —
// where no backup exists and the correct fix is precisely the one it warned against. The
// operator is the only party who knows whether a game server was ever given this public
// key, so the message states both and lets them choose.
func TestTheHalfAPairRefusalOffersTheRecoveryAFirstStartCanActuallyUse(t *testing.T) {
	t.Parallel()

	for name, missing := range map[string]string{
		"the public half is missing":  VerifyingKeyFileName,
		"the private half is missing": SigningKeyFileName,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := t.TempDir()
			if _, err := LoadOrCreate(dir); err != nil {
				t.Fatalf("LoadOrCreate: %v", err)
			}
			if err := os.Remove(filepath.Join(dir, missing)); err != nil {
				t.Fatalf("Remove: %v", err)
			}

			_, err := LoadOrCreate(dir)
			if err == nil {
				t.Fatal("half a pair was accepted")
			}
			message := err.Error()
			for _, want := range []string{
				// Both files, so the operator knows what to look for.
				SigningKeyFileName, VerifyingKeyFileName,
				// The recovery for a pair that has been in service...
				"backup",
				// ...and the one for a first start that did not finish, which is the
				// sentence that was missing.
				"delete",
			} {
				if !strings.Contains(message, want) {
					t.Errorf("the refusal %q does not mention %q", message, want)
				}
			}
		})
	}
}
