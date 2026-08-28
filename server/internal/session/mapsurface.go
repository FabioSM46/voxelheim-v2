package session

import (
	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// The surface vocabulary is one list kept in two packages, and this is where they are
// held to each other.
//
// **Neither side can state the agreement, which is why it is stated here.**
// internal/world must not know that a wire exists and internal/protocol must not know
// how terrain is made, so [world.SurfaceKind] and `MapSurface` are declared apart and
// numbered by hand. This package imports both, so it is the only place the two lists
// are visible at once.
//
// The constants below are the pin: each member is converted in both directions, and a
// conversion of a negative constant to uint8 is a compile error, so a member that moves
// on either side fails the build here rather than mislabelling a pixel. One direction
// would bound the difference from one side only.
const (
	_ = uint8(world.SurfaceUnknown - world.SurfaceKind(vnet.MapSurfaceUnknown))
	_ = uint8(vnet.MapSurfaceUnknown - vnet.MapSurface(world.SurfaceUnknown))
	_ = uint8(world.SurfaceGrass - world.SurfaceKind(vnet.MapSurfaceGrass))
	_ = uint8(vnet.MapSurfaceGrass - vnet.MapSurface(world.SurfaceGrass))
	_ = uint8(world.SurfaceSnow - world.SurfaceKind(vnet.MapSurfaceSnow))
	_ = uint8(vnet.MapSurfaceSnow - vnet.MapSurface(world.SurfaceSnow))
	_ = uint8(world.SurfaceSand - world.SurfaceKind(vnet.MapSurfaceSand))
	_ = uint8(vnet.MapSurfaceSand - vnet.MapSurface(world.SurfaceSand))
	_ = uint8(world.SurfaceStone - world.SurfaceKind(vnet.MapSurfaceStone))
	_ = uint8(vnet.MapSurfaceStone - vnet.MapSurface(world.SurfaceStone))
	_ = uint8(world.SurfaceGravel - world.SurfaceKind(vnet.MapSurfaceGravel))
	_ = uint8(vnet.MapSurfaceGravel - vnet.MapSurface(world.SurfaceGravel))
	_ = uint8(world.SurfaceWater - world.SurfaceKind(vnet.MapSurfaceWater))
	_ = uint8(vnet.MapSurfaceWater - vnet.MapSurface(world.SurfaceWater))
	_ = uint8(world.SurfaceIce - world.SurfaceKind(vnet.MapSurfaceIce))
	_ = uint8(vnet.MapSurfaceIce - vnet.MapSurface(world.SurfaceIce))
	_ = uint8(world.SurfaceForest - world.SurfaceKind(vnet.MapSurfaceForest))
	_ = uint8(vnet.MapSurfaceForest - vnet.MapSurface(world.SurfaceForest))
	_ = uint8(world.SurfaceCave - world.SurfaceKind(vnet.MapSurfaceCave))
	_ = uint8(vnet.MapSurfaceCave - vnet.MapSurface(world.SurfaceCave))
	_ = uint8(world.SurfaceSettlement - world.SurfaceKind(vnet.MapSurfaceSettlement))
	_ = uint8(vnet.MapSurfaceSettlement - vnet.MapSurface(world.SurfaceSettlement))
)

// mapSurfaceOf converts one [world.SurfaceKind] to the wire's `MapSurface`. It is a
// conversion rather than a translation, and the constants above are why.
func mapSurfaceOf(kind world.SurfaceKind) vnet.MapSurface {
	return vnet.MapSurface(kind)
}
