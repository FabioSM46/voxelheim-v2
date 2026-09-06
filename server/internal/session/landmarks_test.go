package session_test

import (
	"slices"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
)

func TestSessionSuppliesTileKnowledgeAndRefusesForgedLandmarks(t *testing.T) {
	m := startMarking(t, t.TempDir(), testAccount(73), "Eivor", 1)
	if slices.Contains(m.sink.kindsReceived(), vnet.PayloadLandmarkList) {
		t.Fatal("welcome sent global landmarks")
	}
	// Ordinary map requests name remote coordinates but must not grow discovery.
	m.place(7091, -93680, "remote place")
	m.waitForLists(t, 2)
	m.conn.in <- protocol.EncodeChunkResendRequest(protocol.ChunkResendRequest{Coord: protocol.ChunkCoord{X: 221, Z: -2928}})
	m.conn.in <- protocol.EncodeMapTileRequest(protocol.MapTileRequest{OriginX: 7040, OriginZ: -93696, Scale: 1})
	waitUntil(t, "masked remote tile and portal scope", func() bool {
		return len(m.sink.mapTilesReceived()) == 1 && slices.Contains(m.sink.kindsReceived(), vnet.PayloadLandmarkList)
	})
	tile := m.sink.mapTilesReceived()[0]
	for i, surface := range tile.Surface {
		if surface != 0 || tile.Height[i] != 0 {
			t.Fatal("unexplored remote terrain revealed")
		}
	}
	// Real admitted session, real encoded authoritative payload. There is no request
	// for a client to forge; sending the server's list ends this connection.
	frame, err := protocol.EncodeLandmarkList(protocol.LandmarkList{OriginX: 7040, OriginZ: -93696, Scale: 1, Landmarks: []protocol.Landmark{{LandmarkID: 9, X: 7091, Z: -93680, Kind: vnet.LandmarkKindPortal}}})
	if err != nil {
		t.Fatal(err)
	}
	m.conn.in <- frame
	select {
	case err := <-m.done:
		m.stopped = true
		if err == nil {
			t.Fatal("client-authored landmark was accepted")
		}
	case <-time.After(patience):
		t.Fatal("forged landmark did not close session")
	}

	count := 0
	for _, kind := range m.sink.kindsReceived() {
		if kind == vnet.PayloadLandmarkList {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("forge elicited %d lists; want only the requested tile's one", count)
	}
}
