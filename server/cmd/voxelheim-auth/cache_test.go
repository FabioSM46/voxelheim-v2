package main

import (
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

// The two expected directives are written out here as literals rather than read from the
// constants they pin. A test that asserts `got == cacheNoStore` passes whatever
// cacheNoStore was last edited to say, which is a test of the compiler; these are the
// values the endpoints are supposed to send, and changing one is supposed to fail here so
// that whoever changed it reads the argument on [cacheDirective] first.
const (
	wantTicketCache    = "no-store"
	wantTicketKeyCache = "public, max-age=60, must-revalidate"
)

// TestTheTicketResponseTellsEverythingOnThePathNotToKeepIt is the bug: a sign-in answers
// with a bearer credential that cannot be revoked, and used to say nothing at all about
// storing it.
//
// The ticket is read out of the body before the header is judged, because a response that
// carried no credential would satisfy a header assertion while proving nothing.
func TestTheTicketResponseTellsEverythingOnThePathNotToKeepIt(t *testing.T) {
	fake := newFakeDiscord(t)
	svc, _ := signInService(t, fake, discard())

	begun := start(t, svc)
	rec := finish(t, svc, begun.State, fake.issue(t, begun.AuthorizeURL), begun.FinishSecret)
	if rec.Code != http.StatusOK {
		t.Fatalf("POST /finish answered %d: %s", rec.Code, rec.Body.String())
	}

	var answer finishResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &answer); err != nil {
		t.Fatalf("reading the finish response: %v", err)
	}
	if answer.SessionTicket == "" {
		t.Fatal("the finish response carried no session ticket, so this test is asserting a header on nothing")
	}

	if got := rec.Header().Get("Cache-Control"); got != wantTicketCache {
		t.Errorf("the response carrying a session ticket answered Cache-Control %q, want %q\n"+
			"a ticket lives eight hours and cannot be revoked, so a stored copy is that account until it expires",
			got, wantTicketCache)
	}
}

// TestTheTicketKeySaysHowLongACopyStaysFresh is the other half, and it is deliberately
// not the same answer.
//
// A game server reads this key once at startup and keeps it, so forbidding a cache from
// storing a value that was published to be copied would be defending the wrong thing. The
// failure that matters is a *stale* copy after a rotation — every player refused by a
// fleet holding a key this service no longer signs with — so the header states a window
// and refuses to let a cache extend it.
func TestTheTicketKeySaysHowLongACopyStaysFresh(t *testing.T) {
	svc := registryService(t, discard())

	rec := call(t, svc, http.MethodGet, "/v1/ticket-key", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /v1/ticket-key answered %d: %s", rec.Code, rec.Body.String())
	}

	got := rec.Header().Get("Cache-Control")
	if got != wantTicketKeyCache {
		t.Errorf("GET /v1/ticket-key answered Cache-Control %q, want %q", got, wantTicketKeyCache)
	}
	// Stated as its own failure because it is the mistake this design is guarding
	// against, and it is the one an author makes while tidying: one directive applied by
	// writeJSON to everything it writes is the change that reads as a simplification and
	// takes the freshness out of this endpoint on the way past.
	if strings.Contains(got, "no-store") {
		t.Errorf("GET /v1/ticket-key answered %q: the key endpoint and the ticket endpoint "+
			"want opposite directives and must not be given one shared answer", got)
	}
}

// TestEveryJSONResponseNamesADirective walks the route table rather than a list of
// endpoints somebody remembered to add to.
//
// Every one of these handlers answers through writeJSON, which takes the directive as a
// parameter — so a handler added later cannot reach this point without a decision having
// been made. What this catches is the decision being made wrongly in the one direction the
// compiler cannot see: a caller that names a directive whose value is empty.
//
// The requests are deliberately unauthenticated and bodiless. Most of these answer a
// refusal, and that is the point: a refusal is a response too, it goes out through the
// same writer, and several of these statuses are ones a cache may assign its own lifetime
// to under RFC 9111 §4.2.2.
func TestEveryJSONResponseNamesADirective(t *testing.T) {
	fake := newFakeDiscord(t)
	svc, _ := signInService(t, fake, discard())

	// health is the one route on the surface that does not write JSON through writeJSON:
	// it writes fixed bytes in main.go and carries no directive today. It is named here
	// rather than silently passing, and the counter below means renaming the route
	// re-opens the question instead of quietly dropping it from this walk.
	const exempt = "GET /healthz"
	skipped := 0

	for _, r := range svc.routes() {
		if r.pattern == exempt {
			skipped++
			continue
		}
		method, path, ok := strings.Cut(r.pattern, " ")
		if !ok {
			t.Fatalf("route %q is not a method and a path", r.pattern)
		}

		rec := call(t, svc, method, path, "")
		if got := rec.Header().Get("Cache-Control"); got == "" {
			t.Errorf("%s answered %d with no Cache-Control", r.pattern, rec.Code)
		}
	}

	if skipped != 1 {
		t.Errorf("the exemption for %q matched %d routes, want exactly 1: "+
			"a route table this test no longer recognises is one it is no longer covering", exempt, skipped)
	}
}
