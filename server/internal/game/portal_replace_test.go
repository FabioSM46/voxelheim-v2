package game

import "testing"

// A remembered visit is replaced only while it still holds exactly the life the caller
// published from, so an expired or resumed visit is never recreated.
func TestReplaceDisconnectedLifeOnlyReplacesTheRememberedValue(t *testing.T) {
	m, _, entry, life := disconnectedPortal(t)
	next := life
	next.Silver++
	stale := life
	stale.Silver += 2

	if m.ReplaceDisconnectedLife(entry.Character, stale, next) {
		t.Fatal("replaced a life that is no longer the remembered one")
	}
	if !m.ReplaceDisconnectedLife(entry.Character, life, next) {
		t.Fatal("did not replace the remembered life")
	}
	restored, visit, err := m.ResumePortal(entry.Character)
	if err != nil || visit == nil || restored == nil || *restored != next {
		t.Fatalf("resume = %+v, %v; want the replacement", restored, err)
	}
	if m.ReplaceDisconnectedLife(entry.Character, next, life) {
		t.Fatal("recreated a visit that had already resumed")
	}
}
