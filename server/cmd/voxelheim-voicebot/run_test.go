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

func TestAnUnknownLogLevelIsRefusedRatherThanPassedOn(t *testing.T) {
	t.Parallel()

	if _, err := parseFlags("test", []string{"-plan", "-server-log-level", "trace"}); err == nil {
		t.Error("-server-log-level trace was accepted; the server would refuse it after this command had started it")
	}
}
