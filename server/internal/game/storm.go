package game

import (
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const (
	// DefaultStormPeriod is the real-week cadence used when the operator supplies no
	// override. Unlike the duration, the cadence is exposed as a server flag.
	DefaultStormPeriod = 168 * time.Hour

	// StormDuration is how long the Fimbulvetr blizzard occupies the whole world.
	// It is a gameplay rule rather than an operator setting.
	StormDuration = 5 * time.Minute
)

var blizzard = protocol.WeatherState{Kind: vnet.WeatherKindBlizzard, Intensity: 255}

// ApproachStorm records the warning a newly joined player must receive. Broadcasts
// remain the wall-clock worker's responsibility; this is only authoritative state.
func (s *Sim) ApproachStorm(secondsUntil uint32) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.stormWarning = protocol.StormWarning{
		Phase:        vnet.StormPhaseApproaching,
		SecondsUntil: secondsUntil,
	}
}

// BeginStorm imposes the global blizzard and records how much of it remains for a
// newly joined player. Repeated calls update the countdown without changing phase.
func (s *Sim) BeginStorm(secondsUntil uint32) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.weatherOverride = &blizzard
	s.stormWarning = protocol.StormWarning{
		Phase:        vnet.StormPhaseRaging,
		SecondsUntil: secondsUntil,
	}
}

// FinishStorm clears the blizzard and queues the bounded regeneration pass. A ward
// protects its whole column, whatever kind of chunk in that column is being examined.
func (s *Sim) FinishStorm(coords []world.Coord) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.weatherOverride = nil
	s.stormWarning = protocol.StormWarning{}
	s.RegenerateChunksLocked(coords, func(column world.Column) bool {
		_, warded := s.wardOf(column)
		return warded
	})
}

// DisableStorm removes live storm state without scouring anything. It is used by
// -storm-period 0, including when a stored deadline exists from an earlier run.
func (s *Sim) DisableStorm() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.weatherOverride = nil
	s.stormWarning = protocol.StormWarning{}
	s.nextStormUnix = 0
}

// StormWarning is the current phase a newly joined player needs, if any.
func (s *Sim) StormWarning() (protocol.StormWarning, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.stormWarning, s.stormWarning.Phase != vnet.StormPhaseUnknown
}
