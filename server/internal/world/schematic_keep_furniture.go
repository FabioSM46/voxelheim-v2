package world

// Stable authored slots1..160 belong to furniture; fixture slots161..256 are separate.
var keepFurnitureProps = []StaticPropPose{
	{Slot: 1, Kind: PropBanquetTable, X: 41, Y: 0, Z: 21, Facing: 2, Variant: 0},    // E0 banquet
	{Slot: 2, Kind: PropBanquetTable, X: 52, Y: 0, Z: 21, Facing: 2, Variant: 0},    // E0 banquet
	{Slot: 3, Kind: PropChair, X: 39, Y: 0, Z: 20, Facing: 3, Variant: 0},           // E0 banquet
	{Slot: 4, Kind: PropChair, X: 40, Y: 0, Z: 20, Facing: 3, Variant: 0},           // E0 banquet
	{Slot: 5, Kind: PropChair, X: 42, Y: 0, Z: 20, Facing: 3, Variant: 0},           // E0 banquet
	{Slot: 6, Kind: PropChair, X: 43, Y: 0, Z: 20, Facing: 3, Variant: 0},           // E0 banquet
	{Slot: 7, Kind: PropChair, X: 39, Y: 0, Z: 22, Facing: 1, Variant: 0},           // E0 banquet
	{Slot: 8, Kind: PropChair, X: 40, Y: 0, Z: 22, Facing: 1, Variant: 0},           // E0 banquet
	{Slot: 9, Kind: PropChair, X: 42, Y: 0, Z: 22, Facing: 1, Variant: 0},           // E0 banquet
	{Slot: 10, Kind: PropChair, X: 43, Y: 0, Z: 22, Facing: 1, Variant: 0},          // E0 banquet
	{Slot: 11, Kind: PropChair, X: 50, Y: 0, Z: 20, Facing: 3, Variant: 0},          // E0 banquet
	{Slot: 12, Kind: PropChair, X: 51, Y: 0, Z: 20, Facing: 3, Variant: 0},          // E0 banquet
	{Slot: 13, Kind: PropChair, X: 53, Y: 0, Z: 20, Facing: 3, Variant: 0},          // E0 banquet
	{Slot: 14, Kind: PropChair, X: 54, Y: 0, Z: 20, Facing: 3, Variant: 0},          // E0 banquet
	{Slot: 15, Kind: PropChair, X: 50, Y: 0, Z: 22, Facing: 1, Variant: 0},          // E0 banquet
	{Slot: 16, Kind: PropChair, X: 51, Y: 0, Z: 22, Facing: 1, Variant: 0},          // E0 banquet
	{Slot: 17, Kind: PropChair, X: 53, Y: 0, Z: 22, Facing: 1, Variant: 0},          // E0 banquet
	{Slot: 18, Kind: PropChair, X: 54, Y: 0, Z: 22, Facing: 1, Variant: 0},          // E0 banquet
	{Slot: 19, Kind: PropCounter, X: 21, Y: 0, Z: 21, Facing: 1, Variant: 1},        // W0 kitchen
	{Slot: 20, Kind: PropCounter, X: 21, Y: 0, Z: 15, Facing: 1, Variant: 1},        // W0 kitchen
	{Slot: 21, Kind: PropBookcase, X: 23, Y: 0, Z: 8, Facing: 4, Variant: 3},        // W0 pantry
	{Slot: 22, Kind: PropBookcase, X: 23, Y: 0, Z: 11, Facing: 4, Variant: 3},       // W0 pantry
	{Slot: 23, Kind: PropBarrel, X: 19, Y: 0, Z: 8, Facing: 1, Variant: 1},          // W0 pantry
	{Slot: 24, Kind: PropBarrel, X: 19, Y: 0, Z: 10, Facing: 1, Variant: 1},         // W0 pantry
	{Slot: 25, Kind: PropBarrel, X: 19, Y: 0, Z: 13, Facing: 1, Variant: 1},         // W0 pantry
	{Slot: 26, Kind: PropBarrel, X: 19, Y: 0, Z: 16, Facing: 1, Variant: 1},         // W0 pantry
	{Slot: 27, Kind: PropBookcase, X: 23, Y: 7, Z: 8, Facing: 4, Variant: 1},        // W7 library
	{Slot: 28, Kind: PropBookcase, X: 23, Y: 7, Z: 11, Facing: 4, Variant: 1},       // W7 library
	{Slot: 29, Kind: PropBookcase, X: 23, Y: 7, Z: 14, Facing: 4, Variant: 1},       // W7 library
	{Slot: 30, Kind: PropBookcase, X: 23, Y: 7, Z: 17, Facing: 4, Variant: 1},       // W7 library
	{Slot: 31, Kind: PropDesk, X: 19, Y: 7, Z: 21, Facing: 1, Variant: 1},           // W7 study
	{Slot: 32, Kind: PropChair, X: 19, Y: 7, Z: 22, Facing: 1, Variant: 1},          // W7 study
	{Slot: 33, Kind: PropDesk, X: 22, Y: 7, Z: 21, Facing: 1, Variant: 1},           // W7 study
	{Slot: 34, Kind: PropChair, X: 22, Y: 7, Z: 22, Facing: 1, Variant: 1},          // W7 study
	{Slot: 35, Kind: PropCouncilTable, X: 21, Y: 14, Z: 21, Facing: 2, Variant: 0},  // W14 council
	{Slot: 36, Kind: PropChair, X: 20, Y: 14, Z: 19, Facing: 3, Variant: 0},         // W14 council
	{Slot: 37, Kind: PropChair, X: 20, Y: 14, Z: 23, Facing: 1, Variant: 0},         // W14 council
	{Slot: 38, Kind: PropChair, X: 22, Y: 14, Z: 19, Facing: 3, Variant: 0},         // W14 council
	{Slot: 39, Kind: PropChair, X: 22, Y: 14, Z: 23, Facing: 1, Variant: 0},         // W14 council
	{Slot: 40, Kind: PropBookcase, X: 23, Y: 14, Z: 9, Facing: 4, Variant: 2},       // W14 council
	{Slot: 41, Kind: PropBench, X: 21, Y: 14, Z: 15, Facing: 2, Variant: 2},         // W14 council
	{Slot: 42, Kind: PropBench, X: 21, Y: 21, Z: 9, Facing: 2, Variant: 2},          // W21 gallery
	{Slot: 43, Kind: PropBench, X: 21, Y: 21, Z: 15, Facing: 2, Variant: 2},         // W21 gallery
	{Slot: 44, Kind: PropDesk, X: 20, Y: 21, Z: 21, Facing: 1, Variant: 2},          // W21 gallery
	{Slot: 45, Kind: PropChair, X: 20, Y: 21, Z: 22, Facing: 1, Variant: 2},         // W21 gallery
	{Slot: 46, Kind: PropThrone, X: 40, Y: 8, Z: 21, Facing: 2, Variant: 0},         // E7 throne
	{Slot: 47, Kind: PropChair, X: 51, Y: 7, Z: 20, Facing: 4, Variant: 0},          // E7 audience
	{Slot: 48, Kind: PropChair, X: 51, Y: 7, Z: 22, Facing: 4, Variant: 0},          // E7 audience
	{Slot: 49, Kind: PropChair, X: 53, Y: 7, Z: 20, Facing: 4, Variant: 0},          // E7 audience
	{Slot: 50, Kind: PropChair, X: 53, Y: 7, Z: 22, Facing: 4, Variant: 0},          // E7 audience
	{Slot: 51, Kind: PropChair, X: 40, Y: 7, Z: 8, Facing: 1, Variant: 0},           // E7 audience
	{Slot: 52, Kind: PropChair, X: 41, Y: 7, Z: 15, Facing: 3, Variant: 0},          // E7 audience
	{Slot: 53, Kind: PropBench, X: 41, Y: 14, Z: 21, Facing: 2, Variant: 1},         // E14 guard
	{Slot: 54, Kind: PropBench, X: 52, Y: 14, Z: 21, Facing: 2, Variant: 1},         // E14 guard
	{Slot: 55, Kind: PropEquipmentRack, X: 39, Y: 14, Z: 23, Facing: 1, Variant: 1}, // E14 guard
	{Slot: 56, Kind: PropEquipmentRack, X: 43, Y: 14, Z: 23, Facing: 1, Variant: 1}, // E14 guard
	{Slot: 57, Kind: PropEquipmentRack, X: 50, Y: 14, Z: 23, Facing: 1, Variant: 1}, // E14 guard
	{Slot: 58, Kind: PropEquipmentRack, X: 54, Y: 14, Z: 23, Facing: 1, Variant: 1}, // E14 guard
	{Slot: 59, Kind: PropCounter, X: 40, Y: 14, Z: 16, Facing: 1, Variant: 1},       // E14 guard
	{Slot: 60, Kind: PropBarrel, X: 41, Y: 14, Z: 8, Facing: 1, Variant: 1},         // E14 guard
	{Slot: 61, Kind: PropBench, X: 41, Y: 21, Z: 21, Facing: 2, Variant: 2},         // E21 gallery
	{Slot: 62, Kind: PropBench, X: 52, Y: 21, Z: 21, Facing: 2, Variant: 2},         // E21 gallery
	{Slot: 63, Kind: PropBookcase, X: 41, Y: 21, Z: 8, Facing: 1, Variant: 2},       // E21 gallery
	{Slot: 64, Kind: PropDesk, X: 40, Y: 21, Z: 16, Facing: 1, Variant: 2},          // E21 gallery
	{Slot: 65, Kind: PropChair, X: 40, Y: 21, Z: 17, Facing: 1, Variant: 2},         // E21 gallery
	{Slot: 66, Kind: PropDesk, X: 40, Y: 28, Z: 21, Facing: 1, Variant: 3},          // E28 overlook
	{Slot: 67, Kind: PropChair, X: 40, Y: 28, Z: 22, Facing: 1, Variant: 3},         // E28 overlook
	{Slot: 68, Kind: PropBench, X: 52, Y: 28, Z: 21, Facing: 2, Variant: 3},         // E28 overlook
	{Slot: 69, Kind: PropBookcase, X: 41, Y: 28, Z: 8, Facing: 1, Variant: 2},       // E28 overlook
	{Slot: 70, Kind: PropBarrel, X: 41, Y: 28, Z: 16, Facing: 1, Variant: 3},        // E28 overlook
	{Slot: 71, Kind: PropDesk, X: 10, Y: 35, Z: 12, Facing: 1, Variant: 2},          // NW scholar
	{Slot: 72, Kind: PropChair, X: 10, Y: 35, Z: 13, Facing: 1, Variant: 2},         // NW scholar
	{Slot: 73, Kind: PropDesk, X: 20, Y: 29, Z: 32, Facing: 1, Variant: 2},          // SW scholar
	{Slot: 74, Kind: PropChair, X: 20, Y: 29, Z: 33, Facing: 1, Variant: 2},         // SW scholar
	{Slot: 75, Kind: PropDesk, X: 50, Y: 41, Z: 12, Facing: 1, Variant: 2},          // NE scholar
	{Slot: 76, Kind: PropChair, X: 50, Y: 41, Z: 13, Facing: 1, Variant: 2},         // NE scholar
	{Slot: 77, Kind: PropDesk, X: 42, Y: 35, Z: 32, Facing: 1, Variant: 2},          // SE scholar
	{Slot: 78, Kind: PropChair, X: 42, Y: 35, Z: 33, Facing: 1, Variant: 2},         // SE scholar
	{Slot: 83, Kind: PropRug, X: 41, Y: 0, Z: 21, Facing: 2, Variant: 0},            // E0 banquet
	{Slot: 84, Kind: PropRug, X: 52, Y: 0, Z: 21, Facing: 2, Variant: 0},            // E0 banquet
	{Slot: 85, Kind: PropRunner, X: 16, Y: 0, Z: 22, Facing: 1, Variant: 0},         // West hall
	{Slot: 86, Kind: PropRunner, X: 16, Y: 7, Z: 22, Facing: 1, Variant: 1},         // West hall
	{Slot: 87, Kind: PropRunner, X: 16, Y: 14, Z: 22, Facing: 1, Variant: 2},        // West hall
	{Slot: 88, Kind: PropRunner, X: 16, Y: 21, Z: 22, Facing: 1, Variant: 3},        // West hall
	{Slot: 89, Kind: PropRug, X: 21, Y: 14, Z: 21, Facing: 2, Variant: 0},           // W14 council
	{Slot: 90, Kind: PropRunner, X: 46, Y: 0, Z: 22, Facing: 1, Variant: 0},         // E0 central aisle
	{Slot: 91, Kind: PropRunner, X: 46, Y: 7, Z: 21, Facing: 2, Variant: 0},         // E7 throne approach
	{Slot: 92, Kind: PropRug, X: 46, Y: 14, Z: 21, Facing: 1, Variant: 2},           // East hall
	{Slot: 93, Kind: PropRug, X: 46, Y: 21, Z: 21, Facing: 1, Variant: 3},           // East hall
	{Slot: 94, Kind: PropRug, X: 46, Y: 28, Z: 21, Facing: 1, Variant: 0},           // East hall

	{Slot: 99, Kind: PropBanner, X: 24, Y: 2, Z: 18, Facing: 4, Variant: 0},   // Attached room wall detail
	{Slot: 100, Kind: PropShield, X: 24, Y: 3, Z: 15, Facing: 4, Variant: 0},  // Attached room wall detail
	{Slot: 101, Kind: PropTrophy, X: 24, Y: 3, Z: 8, Facing: 4, Variant: 0},   // Attached room wall detail
	{Slot: 102, Kind: PropBanner, X: 24, Y: 9, Z: 18, Facing: 4, Variant: 1},  // Attached room wall detail
	{Slot: 103, Kind: PropShield, X: 24, Y: 10, Z: 15, Facing: 4, Variant: 1}, // Attached room wall detail
	{Slot: 104, Kind: PropTrophy, X: 24, Y: 10, Z: 8, Facing: 4, Variant: 1},  // Attached room wall detail
	{Slot: 105, Kind: PropBanner, X: 24, Y: 16, Z: 18, Facing: 4, Variant: 2}, // Attached room wall detail
	{Slot: 106, Kind: PropShield, X: 24, Y: 17, Z: 15, Facing: 4, Variant: 2}, // Attached room wall detail
	{Slot: 107, Kind: PropTrophy, X: 24, Y: 17, Z: 8, Facing: 4, Variant: 2},  // Attached room wall detail
	{Slot: 108, Kind: PropBanner, X: 24, Y: 23, Z: 18, Facing: 4, Variant: 3}, // Attached room wall detail
	{Slot: 109, Kind: PropShield, X: 24, Y: 24, Z: 15, Facing: 4, Variant: 3}, // Attached room wall detail
	{Slot: 110, Kind: PropTrophy, X: 24, Y: 24, Z: 8, Facing: 4, Variant: 3},  // Attached room wall detail
	{Slot: 111, Kind: PropBanner, X: 38, Y: 2, Z: 18, Facing: 2, Variant: 0},  // Attached room wall detail
	{Slot: 112, Kind: PropShield, X: 38, Y: 3, Z: 15, Facing: 2, Variant: 0},  // Attached room wall detail
	{Slot: 113, Kind: PropTrophy, X: 38, Y: 3, Z: 8, Facing: 2, Variant: 0},   // Attached room wall detail
	{Slot: 114, Kind: PropBanner, X: 38, Y: 9, Z: 18, Facing: 2, Variant: 1},  // Attached room wall detail
	{Slot: 115, Kind: PropShield, X: 38, Y: 10, Z: 15, Facing: 2, Variant: 1}, // Attached room wall detail
	{Slot: 116, Kind: PropTrophy, X: 38, Y: 10, Z: 8, Facing: 2, Variant: 1},  // Attached room wall detail
	{Slot: 117, Kind: PropBanner, X: 38, Y: 16, Z: 18, Facing: 2, Variant: 2}, // Attached room wall detail
	{Slot: 118, Kind: PropShield, X: 38, Y: 17, Z: 15, Facing: 2, Variant: 2}, // Attached room wall detail
	{Slot: 119, Kind: PropTrophy, X: 38, Y: 17, Z: 8, Facing: 2, Variant: 2},  // Attached room wall detail
	{Slot: 120, Kind: PropBanner, X: 38, Y: 23, Z: 18, Facing: 2, Variant: 3}, // Attached room wall detail
	{Slot: 121, Kind: PropShield, X: 38, Y: 24, Z: 15, Facing: 2, Variant: 3}, // Attached room wall detail
	{Slot: 122, Kind: PropTrophy, X: 38, Y: 24, Z: 8, Facing: 2, Variant: 3},  // Attached room wall detail
	{Slot: 123, Kind: PropBanner, X: 38, Y: 30, Z: 18, Facing: 2, Variant: 0}, // Attached room wall detail
	{Slot: 124, Kind: PropShield, X: 38, Y: 31, Z: 15, Facing: 2, Variant: 0}, // Attached room wall detail
	{Slot: 125, Kind: PropTrophy, X: 38, Y: 31, Z: 8, Facing: 2, Variant: 0},  // Attached room wall detail
}
