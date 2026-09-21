package world

// One immutable catalogue, combined once during package initialization.
var keepStaticProps = append(append([]StaticPropPose{}, keepFurnitureProps...), keepFixtureProps...)
