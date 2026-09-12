package world

// Authored placements are enabled only after the renderer and final room-route
// acceptance in #1203. Reserving an empty catalogue lets the immutable mechanism
// and its collision/streaming tests ship independently without invisible solids.
var keepStaticProps = []StaticPropPose{}
