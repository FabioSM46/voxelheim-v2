package main

import (
	"context"
	"fmt"
	"github.com/FabioSM46/voxelheim-v2/server/internal/protocol"
	"github.com/FabioSM46/voxelheim-v2/server/internal/transport"
	"math"
	"net"
	"strings"
	"testing"
	"time"

	vnet "github.com/FabioSM46/voxelheim-v2/server/gen/Voxelheim/Net"
	"github.com/FabioSM46/voxelheim-v2/server/internal/world"
)

// flatView is one delivered chunk: a stone floor at y = 0 and air above it.
func flatView() *blockView {
	v := newBlockView()
	blocks := make([]world.Block, world.ChunkSize*world.ChunkSize*world.ChunkSize)
	for z := range world.ChunkSize {
		for x := range world.ChunkSize {
			blocks[world.Index(x, 0, z)] = world.Stone
		}
	}
	v.chunks[world.Coord{}] = blocks
	return v
}

func TestThePathWalksAroundAWallAndClimbsOneCourse(t *testing.T) {
	v := flatView()
	for z := int64(0); z < 10; z++ {
		v.set(5, 1, z, world.Stone)
		v.set(5, 2, z, world.Stone)
	}
	route := v.path(cell{2, 1, 2}, exactly(cell{8, 1, 2}))
	if route == nil || route[len(route)-1] != (cell{8, 1, 2}) {
		t.Fatalf("no route around the wall: %v", route)
	}
	for _, c := range route {
		if c[0] == 5 && c[2] < 10 {
			t.Fatalf("the route walks through the wall at %v", c)
		}
	}
	v.set(3, 1, 2, world.Stone) // a one-course step
	up := v.path(cell{2, 1, 2}, exactly(cell{3, 2, 2}))
	if len(up) != 1 {
		t.Fatalf("one step up takes %v", up)
	}
}

func TestUnknownTerrainAndPortalsAreNotWalkable(t *testing.T) {
	v := flatView()
	if v.path(cell{2, 1, 2}, exactly(cell{40, 1, 2})) != nil {
		t.Fatal("a route crossed a chunk that was never delivered")
	}
	for z := int64(0); z < world.ChunkSize; z++ {
		v.set(6, 1, z, world.PortalVeil)
		v.set(6, 2, z, world.PortalVeil)
	}
	if v.path(cell{2, 1, 2}, exactly(cell{9, 1, 2})) != nil {
		t.Fatal("a route walked through a portal's veil")
	}
}

func TestYawTowardMatchesTheServersBasis(t *testing.T) {
	// yaw 0 looks along -Z and forward is (-sin yaw, -cos yaw).
	for _, d := range [][2]float64{{0, -1}, {1, 0}, {0, 1}, {-1, 0}, {3, 4}} {
		yaw := yawToward(d[0], d[1])
		length := math.Hypot(d[0], d[1])
		if fx, fz := -math.Sin(yaw), -math.Cos(yaw); math.Abs(fx-d[0]/length) > 1e-9 || math.Abs(fz-d[1]/length) > 1e-9 {
			t.Fatalf("yaw %.3f walks along (%.3f, %.3f), not %v", yaw, fx, fz, d)
		}
	}
}

func TestTheSoloSiegeIsTwelvePacksOfThree(t *testing.T) {
	for members, want := range map[int]int{1: 36, 2: 36, 3: 36, 4: 48, 5: 60} {
		if got := expectedSpiders(members); got != want {
			t.Fatalf("a party of %d faces %d spiders, want %d", members, got, want)
		}
	}
}

func TestTheHumanEstimateReplacesOnlyTheBossFights(t *testing.T) {
	got, ok := humanEstimate(3, 20*time.Minute, 5*time.Minute, 6*time.Minute)
	want := 9*time.Minute + time.Duration((206.65+324.70)*float64(time.Second))
	if d := got - want; !ok || d < -time.Millisecond || d > time.Millisecond {
		t.Fatalf("estimate %v (%t), want %v", got, ok, want)
	}
	if _, ok := humanEstimate(2, 20*time.Minute, 0, 0); ok {
		t.Fatal("a pair has an estimate, but no reader was ever measured at two")
	}
}

func TestTheBarrierReleasesOnlyWhenEveryMemberHasArrived(t *testing.T) {
	b := newBarrier(3)
	first, second := b.arrive(), b.arrive()
	select {
	case <-first:
		t.Fatal("released with two of three members")
	default:
	}
	third := b.arrive()
	for i, ch := range []<-chan struct{}{first, second, third} {
		select {
		case <-ch:
		default:
			t.Fatalf("arrival %d was not released once all three had arrived", i)
		}
	}
	// The next part of the route starts a fresh count.
	select {
	case <-b.arrive():
		t.Fatal("the barrier's next round released at once")
	default:
	}
}

func TestFlagsRefuseARunWithNoServer(t *testing.T) {
	if _, err := parseFlags("bot", nil); err == nil {
		t.Fatal("a run with no -server was accepted")
	}
	if _, err := parseFlags("bot", []string{"-server", "voxelheimd", "-max-wipes", "0"}); err == nil {
		t.Fatal("-max-wipes 0 was accepted")
	}
	for _, n := range []string{"0", "6"} {
		if _, err := parseFlags("bot", []string{"-server", "voxelheimd", "-party", n}); err == nil {
			t.Fatalf("-party %s was accepted", n)
		}
	}
	o, err := parseFlags("bot", []string{"-server", "voxelheimd"})
	if err != nil || o.maxWipes != 3 || o.viewDistance != 4 || o.members != 3 || o.level != 1 {
		t.Fatalf("defaults %+v, %v", o, err)
	}
}

func TestTheReaderSeesTheRegionsTheServerStrikes(t *testing.T) {
	disc := region{shape: vnet.HazardShapeDisc, origin: [3]float64{0, 10, 0}, radius: 3, height: 4}
	lane := region{shape: vnet.HazardShapeLine, origin: [3]float64{0, 10, 0}, direction: [3]float64{1, 0, 0},
		radius: 8, height: 4, halfWidth: 1}
	for _, c := range []struct {
		what    string
		regions []region
		pos     [3]float64
		want    bool
	}{
		{"inside the disc", []region{disc}, [3]float64{1, 10, 1}, true},
		{"a margin past its rim", []region{disc}, [3]float64{3.5, 10, 0}, true},
		{"well clear of it", []region{disc}, [3]float64{5, 10, 0}, false},
		{"a floor above it", []region{disc}, [3]float64{0, 13, 0}, false},
		{"on the lane", []region{lane}, [3]float64{6, 10, 0.5}, true},
		{"behind the lane's start", []region{lane}, [3]float64{-2, 10, 0}, false},
		{"beside the lane", []region{lane}, [3]float64{4, 10, 2.5}, false},
		{"nothing announced", nil, [3]float64{0, 10, 0}, false},
	} {
		if got := touches(c.regions, c.pos); got != c.want {
			t.Errorf("%s: touches %t, want %t", c.what, got, c.want)
		}
	}
}

func TestAWalkAlongTheWorldBecomesTheControlsForTheFacing(t *testing.T) {
	yaw := yawToward(1, 0) // facing +X
	if x, z := relative([2]float64{1, 0}, yaw); math.Abs(x) > 1e-9 || math.Abs(z-1) > 1e-9 {
		t.Fatalf("walking the way the body faces is (%.3f, %.3f), want forward", x, z)
	}
	if x, z := relative([2]float64{0, 1}, yaw); math.Abs(math.Abs(x)-1) > 1e-9 || math.Abs(z) > 1e-9 {
		t.Fatalf("walking across the facing is (%.3f, %.3f), want a pure strafe", x, z)
	}
}

func TestLevelFlagRefusesValuesOutsideTheServerCurve(t *testing.T) {
	for _, value := range []string{"0", "-1", "31", "65537", "bad", "1.5"} {
		if _, err := parseFlags("bot", []string{"-server", "voxelheimd", "-level", value}); err == nil {
			t.Errorf("accepted -level %s", value)
		}
	}
	for _, level := range []int{1, 2, 21, 30} {
		o, err := parseFlags("bot", []string{"-server", "voxelheimd", "-level", fmt.Sprint(level)})
		if err != nil || o.level != level {
			t.Fatalf("level %d parsed as %d: %v", level, o.level, err)
		}
	}
}

func TestLevelOneNeverSendsAnExperienceCommand(t *testing.T) {
	// There is deliberately no connection: level 1 must perform no write or read.
	r := &runner{opts: options{level: 1}}
	if err := r.prepareLevel(context.Background()); err != nil {
		t.Fatal(err)
	}
}

func TestExperienceProvisioningRequiresTheAuthoritativeLevel(t *testing.T) {
	for _, state := range []string{"confirmed", "refused", "too high", "unconfirmed"} {
		t.Run(state, func(t *testing.T) {
			local, remote := net.Pipe()
			defer func() { _ = local.Close(); _ = remote.Close() }()
			stats := newRunStats(newTally())
			c := &client{conn: local, level: 1, stats: stats, chat: make(chan string, 1)}
			r := &runner{pilot: &pilot{c: c}, opts: options{level: 21}}
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			defer cancel()
			result := make(chan error, 1)
			go func() { result <- r.prepareLevel(ctx) }()
			if err := remote.SetReadDeadline(time.Now().Add(time.Second)); err != nil {
				t.Fatal(err)
			}
			frame, err := transport.ReadFrame(remote)
			if err != nil {
				t.Fatal(err)
			}
			message, err := protocol.Decode(frame)
			if err != nil || message.Chat == nil || message.Chat.Text != "/addexperience 10500" {
				t.Fatalf("request %+v, %v", message, err)
			}
			if state == "refused" {
				c.chat <- "Development commands are disabled."
			} else {
				c.chat <- "Added 10500 experience; total 10500, level 21."
				select {
				case err := <-result:
					t.Fatalf("finished without authoritative vitals: %v", err)
				default:
				}
				if state == "unconfirmed" {
					cancel()
				} else {
					level := uint16(21)
					if state == "too high" {
						level++
					}
					c.mu.Lock()
					c.level = level
					c.mu.Unlock()
				}
			}
			if err := <-result; (err == nil) != (state == "confirmed") {
				t.Fatalf("%s: %v", state, err)
			}
			if len(stats.commands) != 1 || stats.commands[0] != "/addexperience 10500" {
				t.Fatalf("command accounting: %v", stats.commands)
			}
		})
	}
}

func TestReportStatesStartingLevelAndCountsExperienceGrants(t *testing.T) {
	for _, level := range []int{1, 21} {
		var members []*runner
		for i := range 3 {
			stats := newRunStats(newTally())
			stats.command("/teleport 1 2 3")
			stats.command("/additem 1 1")
			if level > 1 {
				stats.command("/addexperience 10500")
			}
			members = append(members, &runner{
				pilot: &pilot{c: &client{name: memberNames[i]}, stats: stats},
				opts:  options{level: level}, server: &serverProcess{commandLine: "fixture"},
			})
		}
		var report strings.Builder
		writeReport(&report, newParty(members), nil)
		text := report.String()
		if !strings.Contains(text, fmt.Sprintf("party starting level: %d\n", level)) {
			t.Fatal("report omitted party level")
		}
		if level == 1 {
			if !strings.Contains(text, "development commands sent: 6 (/immortal: 0)\n") || strings.Contains(text, "development command uses:") {
				t.Fatal("level 1 changed the command report")
			}
		} else if !strings.Contains(text, "development commands sent: 9 (/immortal: 0)\n") ||
			!strings.Contains(text, "development command uses: /teleport: 3, /additem: 3, /addexperience: 3\n") {
			t.Fatal("report lost development command counts")
		}
	}
}
