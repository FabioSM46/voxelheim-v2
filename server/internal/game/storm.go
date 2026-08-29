package game

import (
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

const (
	// DefaultStormPeriod is the operator-overridable real-week cadence.
	DefaultStormPeriod = 168 * time.Hour

	// StormDuration is the fixed global-blizzard duration.
	StormDuration = 5 * time.Minute
)

var blizzard = protocol.WeatherState{Kind: vnet.WeatherKindBlizzard, Intensity: 255}

func (s *Sim) ApproachStorm(secondsUntil uint32) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.stormWarning = protocol.StormWarning{
		Phase:        vnet.StormPhaseApproaching,
		SecondsUntil: secondsUntil,
	}
}

func (s *Sim) BeginStorm(secondsUntil uint32) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.weatherOverride = &blizzard
	s.stormWarning = protocol.StormWarning{
		Phase:        vnet.StormPhaseRaging,
		SecondsUntil: secondsUntil,
	}
}

func (s *Sim) StartStormRegeneration(coords []world.Coord) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.RegenerateChunksLocked(coords, func(column world.Column) bool {
		// Keep on the boolean, never on the owner: a settlement is a ward owned
		// by the zero identity, which deliberately names nobody.
		_, warded := s.wardOf(column)
		return warded
	})
}

func (s *Sim) StormRegenerationComplete() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.regeneration) == 0
}

func (s *Sim) CompleteStorm() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.regeneration) != 0 {
		return false
	}
	s.weatherOverride = nil
	s.stormWarning = protocol.StormWarning{}
	return true
}

// DisableStorm removes live state without healing, including a stored deadline.
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
