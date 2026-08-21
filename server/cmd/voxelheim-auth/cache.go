package main

// cacheDirective is the `Cache-Control` value a response carries.
//
// **It is a parameter of [service.writeJSON] rather than something that function
// decides, because the two responses that most need one want opposite things.**
// `POST /v1/signin/discord/finish` hands back a bearer ticket with an eight-hour life
// and no revocation — whatever holds those bytes is that account until they expire — so
// nothing on the path may keep a copy. `GET /v1/ticket-key` publishes the verifying key
// a game server reads once at startup and then keeps; it is not secret, and the failure
// that matters there is a *stale* copy, which is a fleet refusing every player it should
// admit. A single directive applied by the writer to everything it writes would have to
// be wrong for one of those two.
//
// So the writer takes the directive and every caller names one. That is the point of the
// parameter: a handler added later does not inherit whatever suited the endpoint beside
// it, because it does not compile until somebody has chosen.
//
// **The value being absent is a choice too, and it was the wrong one.** RFC 9111 §4.2.2
// lets a cache invent its own freshness for a response that carries no explicit
// directive, so "we sent no header" is not "nobody may store this" — it is "everybody
// may guess". `net/http` sends none of its own.
type cacheDirective string

const (
	// cacheNoStore forbids every cache on the path — shared, private, and the client's
	// own disk — from keeping any part of the response. RFC 9111 §5.2.2.5.
	//
	// **`no-store` rather than `private`**, because the thing being written down is the
	// credential itself and `private` still permits the requesting client's own store.
	// There is nothing to trade away by being strict: none of the responses that carry
	// this is a document anybody re-reads, so a cache that honoured it would save one
	// round trip that nobody makes.
	//
	// It is the answer everywhere in this service except the ticket key, and the reason
	// is written at each call site — the ticket is the obvious one, and the sign-in state,
	// the server list and every refusal are the ones worth arguing for out loud.
	cacheNoStore cacheDirective = "no-store"

	// cacheTicketKeyFreshness is the deliberate freshness on `GET /v1/ticket-key`, and it
	// is a permission rather than a prohibition on purpose.
	//
	// **The risk here is a stale copy, not a stored one.** The key is public by
	// construction — the endpoint is unauthenticated because a game server that had to
	// authenticate to read it would need a credential from this service before it could
	// stop depending on this service. So `no-store` would be protecting a value from
	// disclosure it was published for, and it would still not be the interesting
	// question, which is what happens after the pair changes. Rotation is a known gap
	// (see server/AGENTS.md): deleting the pair is the whole of the ceremony today, and
	// an intermediary serving the old key afterwards is every player refused, with
	// nothing in any log saying why.
	//
	// **One minute, chosen because caching buys close to nothing here.** A game server
	// reads this once at startup and keeps it, so the requests are a fleet booting and an
	// operator running curl; a window long enough to coalesce a simultaneous restart is
	// the whole of the benefit, and every second past that is only more time in which a
	// rotated key can be answered with the old one. It is a number this service can
	// afford to be wrong about in the cheap direction.
	//
	// `must-revalidate` because "fresh for a minute" and "may be served stale when the
	// origin is unreachable" are different claims, and only the first is being made: a
	// cache must not reuse this once it is stale, which also declines the
	// `stale-if-error` behaviour a cache would otherwise be free to apply (RFC 9111
	// §5.2.2.2). `public` states in the header what the doc comment states in prose —
	// this one is shareable, unlike everything else here — and is what would keep it
	// shareable if a deployment ever put it behind a gateway that authenticated
	// (RFC 9111 §3.5).
	cacheTicketKeyFreshness cacheDirective = "public, max-age=60, must-revalidate"
)
