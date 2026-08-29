package main

import (
	"context"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/game"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

const (
	warnedTenMinutes uint8 = 1 << iota
	warnedOneMinute
	warnedTenSeconds
)

type stormCycle struct {
	deadline int64
	warned   uint8
	seen     bool
	raging   bool
}

// stormLoop checks immediately at startup and then every ten seconds. An immediate
// pass is what resumes a storm after a restart instead of leaving ordinary weather in
// place until the first ticker edge.
func (s *server) stormLoop(ctx context.Context) error {
	if s.stormPeriod == 0 {
		s.sim.DisableStorm()
		return nil
	}
	clock := s.wallClock
	if clock == nil {
		clock = game.SystemClock{}
	}
	every := s.stormEvery
	if every <= 0 {
		every = stormPollInterval
	}

	next := clock.Now()
	for {
		s.stormPass(clock.Now())
		next = next.Add(every)
		if err := clock.SleepUntil(ctx, next); err != nil {
			return err
		}
	}
}

// stormPass is one wall-clock decision. It is kept separate from the wait so a fake
// clock can drive every boundary exactly and so none of these rules enters the tick.
func (s *server) stormPass(now time.Time) {
	if s.stormPeriod == 0 {
		return
	}
	nowUnix := now.Unix()
	due := s.sim.NextStorm()
	if due == 0 {
		due = nowUnix + durationSeconds(s.stormPeriod)
		s.sim.ScheduleStorm(due)
		s.stormCycle = stormCycle{deadline: due, seen: true}
		s.flushClock()
		s.log.Info("Fimbulvetr scheduled", "next_storm_unix", due)
		return
	}

	if s.stormCycle.deadline != due {
		s.stormCycle = stormCycle{deadline: due}
	}

	if nowUnix < due {
		s.approachStorm(due - nowUnix)
		return
	}

	end := due + durationSeconds(game.StormDuration)
	if nowUnix < end {
		remaining := uint32(end - nowUnix)
		s.sim.BeginStorm(remaining)
		if !s.stormCycle.raging {
			s.stormCycle.raging = true
			s.broadcastStorm(vnet.StormPhaseRaging, remaining)
			s.log.Info("Fimbulvetr began", "storm_deadline_unix", due)
		}
		return
	}

	if !s.stormCycle.raging {
		// The process was not here for the deadline. Give everybody one useful minute
		// instead of silently scouring a world the instant it restarts.
		newDue := nowUnix + durationSeconds(missedStormWarning)
		s.sim.ScheduleStorm(newDue)
		s.sim.ApproachStorm(uint32(durationSeconds(missedStormWarning)))
		s.stormCycle = stormCycle{
			deadline: newDue,
			warned:   warnedTenMinutes | warnedOneMinute,
			seen:     true,
		}
		s.broadcastStorm(vnet.StormPhaseApproaching, uint32(durationSeconds(missedStormWarning)))
		s.flushClock()
		s.log.Warn("a Fimbulvetr deadline was missed; the storm will begin after one warning",
			"missed_deadline_unix", due, "next_storm_unix", newDue)
		return
	}

	coords, err := s.chunks.RegenerationChunks()
	if err != nil {
		// Keep the blizzard in place and retry. Clearing it without being able to name
		// what must heal would announce a storm that did not finish its only outcome.
		s.sim.BeginStorm(1)
		s.log.Error("listing chunks for Fimbulvetr failed; the storm remains active", "error", err)
		return
	}
	s.sim.FinishStorm(coords)
	s.broadcastStorm(vnet.StormPhasePassed, 0)
	nextDue := due + durationSeconds(s.stormPeriod)
	s.sim.ScheduleStorm(nextDue)
	s.flushClock()
	s.log.Info("Fimbulvetr passed", "chunks_considered", len(coords), "next_storm_unix", nextDue)
	s.stormCycle = stormCycle{deadline: nextDue, seen: true}
}

func (s *server) approachStorm(remaining int64) {
	if remaining > durationSeconds(stormWarningTenMin) {
		s.stormCycle.seen = true
		return
	}

	s.sim.ApproachStorm(uint32(remaining))
	bit := warnedTenMinutes
	prior := uint8(0)
	switch {
	case remaining <= durationSeconds(stormWarningFinal):
		bit, prior = warnedTenSeconds, warnedTenMinutes|warnedOneMinute
	case remaining <= durationSeconds(stormWarningOneMin):
		bit, prior = warnedOneMinute, warnedTenMinutes
	}
	if !s.stormCycle.seen {
		// On restart announce only the phase that is useful now, not every threshold
		// crossed while the server was down.
		s.stormCycle.warned |= prior
	}
	s.stormCycle.seen = true
	if s.stormCycle.warned&bit != 0 {
		return
	}
	s.stormCycle.warned |= bit
	s.broadcastStorm(vnet.StormPhaseApproaching, uint32(remaining))
}

func (s *server) broadcastStorm(phase vnet.StormPhase, seconds uint32) {
	delivered, dropped := s.sim.Broadcast(protocol.EncodeStormWarning(protocol.StormWarning{
		Phase: phase, SecondsUntil: seconds,
	}))
	s.log.Debug("storm phase broadcast", "phase", phase.String(), "seconds_until", seconds,
		"delivered", delivered, "dropped", dropped)
}

func durationSeconds(duration time.Duration) int64 {
	seconds := int64(duration / time.Second)
	if duration%time.Second != 0 {
		seconds++
	}
	return max(seconds, 1)
}
