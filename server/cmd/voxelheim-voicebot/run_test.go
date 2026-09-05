package main

import (
	"strings"
	"testing"
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

func TestARunIsRefusedWithoutTheServerItMeasures(t *testing.T) {
	t.Parallel()

	if _, err := parseFlags("test", []string{"-sessions", "100"}); err == nil {
		t.Error("a run with no -server was accepted; this command starts the server it reads /proc for")
	}
	// A plan needs no server, because it connects nothing.
	call, err := parseFlags("test", []string{"-plan"})
	if err != nil || !call.plan {
		t.Errorf("a plan was refused: plan=%v err=%v", call.plan, err)
	}
}

func TestTheRunFlagsRefuseAWindowThatWouldCountUnsettledFrames(t *testing.T) {
	t.Parallel()

	_, err := parseFlags("test", []string{"-plan", "-settle", "1ms"})
	if err == nil || !strings.Contains(err.Error(), "recompute of the audible sets") {
		t.Errorf("a settle shorter than one audible-set recompute was accepted: %v", err)
	}
	if got := audibleSetSettle(20); got != time.Duration(game.VoiceSetInterval)*50*time.Millisecond {
		t.Errorf("one recompute at 20 Hz is %v, want %v", got, time.Duration(game.VoiceSetInterval)*50*time.Millisecond)
	}
}

func TestACeilingBelowTheSessionsIsRefusedAndZeroMeansTheFloor(t *testing.T) {
	t.Parallel()

	if _, err := parseFlags("test", []string{"-plan", "-sessions", "200", "-max-players", "150"}); err == nil {
		t.Error("a ceiling of 150 was accepted for 200 sessions")
	}
	call, err := parseFlags("test", []string{"-plan", "-sessions", "10", "-clusters", "1", "-cluster-radius", "2"})
	if err != nil {
		t.Fatalf("a ten-session plan was refused: %v", err)
	}
	if call.run.maxPlayers != session.MinConcurrentSessions {
		t.Errorf("a ten-session run asks for a ceiling of %d, want the server's floor %d",
			call.run.maxPlayers, session.MinConcurrentSessions)
	}
}

// **A level the server accepts is not automatically one this command can use.** `warn` and
// `error` are legal for voxelheimd and silence the two Info startup lines the address and
// the certificate are read out of, so a run given one would start a server it could never
// find. #935's review is where that was caught.
func TestALogLevelThisCommandCannotReadTheServerThroughIsRefused(t *testing.T) {
	t.Parallel()

	for _, level := range []string{"trace", "warn", "error", "INFO", ""} {
		if _, err := parseFlags("test", []string{"-plan", "-server-log-level", level}); err == nil {
			t.Errorf("-server-log-level %q was accepted", level)
		}
	}
	for _, level := range []string{"debug", "info"} {
		if _, err := parseFlags("test", []string{"-plan", "-server-log-level", level}); err != nil {
			t.Errorf("-server-log-level %q was refused: %v", level, err)
		}
	}
}
