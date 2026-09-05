package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// The server under test, started by this command rather than found by it.
//
// **The soak owns the server because the measurement is about the server's cost**, and a
// cost is only attributable to a process you can name. Starting it here gives the run a
// process id to read /proc for, a certificate fingerprint straight out of the startup line
// instead of copied by hand, a listening address chosen by the kernel so two runs never
// collide on a port, and one command in the ADR instead of a page of shell.
//
// Nothing about the server's configuration is invented here: every flag below is one this
// command was given, and the resulting command line is printed in the report so a run can
// be repeated without this file.

// The startup lines this command waits for. Both are Info, so they arrive at any log level
// the run is given.
const (
	listeningMessage   = "voxelheimd listening"
	certificateMessage = "listening with an encrypted session"
)

// The Debug lines internal/game/voice.go writes for each refusal it makes silently on the
// wire. They are the only way to tell one class of drop from another, because the relay
// answers nothing and exports no counter — see the report for what this can and cannot be
// asked for, and why a run that wants them has to be told to log at Debug.
const (
	limiterDropMessage  = "voice frame dropped: the speaker is over the frame rate"
	sizeCapDropMessage  = "voice frame dropped: the payload is longer than the contract allows"
	audienceDropMessage = "voice frame dropped: the audience is not one this server can apply"
	laneDropMessage     = "voice frame dropped: the session's latency lane is full"
	behindMessage       = "simulation fell behind; abandoning missed ticks"
)

// serverProcess is one running voxelheimd and everything the report reads off it.
type serverProcess struct {
	cmd         *exec.Cmd
	commandLine string
	addr        string
	fingerprint string

	// The counters the log scanner fills. Atomic because it runs on its own goroutine for
	// the whole life of the process and the report reads them at the end.
	limiterDrops  atomic.Uint64
	sizeCapDrops  atomic.Uint64
	audienceDrops atomic.Uint64
	laneDrops     atomic.Uint64
	fellBehind    atomic.Uint64

	// logLines is every line the scanner saw, so a report can say whether the drop counts
	// above are zero because nothing was dropped or because nothing was logged.
	logLines atomic.Uint64

	scanned sync.WaitGroup
	tail    *ringTail
}

// serverArgs is the command line, built once so the report can print exactly what ran.
func serverArgs(o options, run runOptions) []string {
	return []string{
		"-listen", "127.0.0.1:0",
		// An ephemeral world: no player records to write, no chunk deltas to save and a
		// fresh certificate every start. A soak test that persisted a thousand characters
		// would be measuring the store.
		"-world-dir=",
		"-world-name", o.worldName,
		"-ticket-key", run.ticketKey,
		"-seed", strconv.FormatInt(o.seed, 10),
		"-max-players", strconv.Itoa(run.maxPlayers),
		"-tick-rate", strconv.Itoa(run.tickRate),
		"-view-distance", strconv.Itoa(run.viewDistance),
		"-voice-range", strconv.FormatFloat(o.voiceRange, 'f', -1, 64),
		// The clusters are made with /teleport, which is a development command and refuses
		// to run without this.
		"-dev-commands",
		"-log-format", "json",
		"-log-level", run.serverLogLevel,
	}
}

// startServer launches voxelheimd and waits for it to say where it is listening and which
// certificate it is presenting.
func startServer(ctx context.Context, o options, run runOptions) (*serverProcess, error) {
	args := serverArgs(o, run)
	cmd := exec.CommandContext(ctx, run.serverBin, args...) //nolint:gosec // the binary is the operator's own flag.
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("open the server's log: %w", err)
	}
	cmd.Stdout = io.Discard
	// The server is killed by name at the end of the run; SIGKILL rather than a graceful
	// stop because a soak test has nothing to persist and a shutdown that waits on a
	// thousand leave lingers is ten seconds of nothing.
	cmd.Cancel = func() error { return cmd.Process.Kill() }

	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start %s: %w", run.serverBin, err)
	}

	server := &serverProcess{
		cmd:         cmd,
		commandLine: strings.Join(append([]string{run.serverBin}, args...), " "),
		tail:        newRingTail(20),
	}
	ready := make(chan error, 1)
	server.scanned.Add(1)
	go server.scan(stderr, ready)

	select {
	case err := <-ready:
		if err != nil {
			_ = cmd.Process.Kill()
			return nil, err
		}
		return server, nil
	case <-ctx.Done():
		_ = cmd.Process.Kill()
		return nil, ctx.Err()
	}
}

// scan reads the server's log for the whole of its life.
//
// Two jobs in one goroutine because they are the same stream: the two startup lines the
// run needs before it can dial, and the running count of the refusals the relay makes
// silently. A line that is not JSON is counted and kept for the tail rather than treated
// as an error — a panic or a runtime message is exactly the thing a failing run needs to
// be able to print.
func (s *serverProcess) scan(stderr io.Reader, ready chan<- error) {
	defer s.scanned.Done()

	scanner := bufio.NewScanner(stderr)
	scanner.Buffer(make([]byte, 0, 64<<10), 1<<20)

	var haveAddr, haveFingerprint, announced bool
	for scanner.Scan() {
		line := scanner.Text()
		s.logLines.Add(1)
		s.tail.push(line)

		var record struct {
			Msg         string `json:"msg"`
			Addr        string `json:"addr"`
			Fingerprint string `json:"certificate_sha256"`
		}
		if err := json.Unmarshal([]byte(line), &record); err != nil {
			continue
		}
		switch record.Msg {
		case listeningMessage:
			s.addr, haveAddr = record.Addr, true
		case certificateMessage:
			s.fingerprint, haveFingerprint = record.Fingerprint, true
		case limiterDropMessage:
			s.limiterDrops.Add(1)
		case sizeCapDropMessage:
			s.sizeCapDrops.Add(1)
		case audienceDropMessage:
			s.audienceDrops.Add(1)
		case laneDropMessage:
			s.laneDrops.Add(1)
		case behindMessage:
			s.fellBehind.Add(1)
		}
		if haveAddr && haveFingerprint && !announced {
			announced = true
			ready <- nil
		}
	}
	if !announced {
		// The stream ended before the server said it was listening: it refused a flag, or
		// it died. The tail is what says which, and it is the whole diagnostic.
		ready <- fmt.Errorf("the server stopped before it was listening:\n%s", s.tail.text())
	}
}

// stop kills the server and waits for it, returning the CPU time the kernel charged it.
//
// **The rusage total is a cross-check on the sampled figure**, not a second measurement of
// the same thing: it covers the process's whole life, joins and teardown included, while
// the sampler covers the measured window alone. Two numbers that disagree by more than the
// join phase explains mean the sampler is wrong.
func (s *serverProcess) stop() (user, system time.Duration) {
	if s.cmd.Process != nil {
		_ = s.cmd.Process.Kill()
	}
	_ = s.cmd.Wait()
	s.scanned.Wait()
	if state := s.cmd.ProcessState; state != nil {
		return state.UserTime(), state.SystemTime()
	}
	return 0, 0
}

func (s *serverProcess) pid() int {
	if s.cmd.Process == nil {
		return 0
	}
	return s.cmd.Process.Pid
}

// ringTail keeps the last few log lines, so a failure can be explained without keeping the
// whole log of a run that writes millions of them.
type ringTail struct {
	mu    sync.Mutex
	lines []string
	next  int
	full  bool
}

func newRingTail(size int) *ringTail { return &ringTail{lines: make([]string, size)} }

func (r *ringTail) push(line string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.lines[r.next] = line
	r.next++
	if r.next == len(r.lines) {
		r.next, r.full = 0, true
	}
}

func (r *ringTail) text() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	var out strings.Builder
	if r.full {
		for _, line := range r.lines[r.next:] {
			out.WriteString(line + "\n")
		}
	}
	for _, line := range r.lines[:r.next] {
		out.WriteString(line + "\n")
	}
	return out.String()
}

// usage is one reading of what a process costs.
type usage struct {
	cpuSeconds float64
	rssBytes   uint64
}

// clockTicksPerSecond is the unit /proc/<pid>/stat reports processor time in.
//
// **It is an assumption and it is stated rather than hidden.** `USER_HZ` is a compile-time
// constant of the kernel's userspace ABI, it is 100 on every Linux this project runs on,
// and the portable way to ask for it is `sysconf(_SC_CLK_TCK)` — which is cgo, and this
// module has no C in it. The cross-check in [serverProcess.stop] is what catches the day
// that stops being true: the kernel's own rusage does not go through this constant.
const clockTicksPerSecond = 100

// readUsage reads one process's processor time and resident size out of /proc.
//
// Linux only, and it says so by failing rather than by a build tag: this command is a load
// generator for a server whose CI, deployment and every measurement in the ADR are Linux,
// and a build tag would drop the file out of vet and the cross-compile gates for the sake
// of a platform nobody runs it on.
func readUsage(pid int) (usage, error) {
	stat, err := os.ReadFile(fmt.Sprintf("/proc/%d/stat", pid))
	if err != nil {
		return usage{}, fmt.Errorf("read the process's processor time: %w", err)
	}
	// The second field is the executable name in parentheses and may itself contain
	// spaces, so the fields after it are counted from the last close parenthesis rather
	// than from the start of the line.
	commandEnd := strings.LastIndexByte(string(stat), ')')
	if commandEnd < 0 {
		return usage{}, errors.New("the process's stat line has no command field")
	}
	fields := strings.Fields(string(stat)[commandEnd+1:])
	// utime and stime are the fourteenth and fifteenth fields of the line, which is the
	// twelfth and thirteenth after the command.
	const utimeIndex, stimeIndex = 11, 12
	if len(fields) <= stimeIndex {
		return usage{}, errors.New("the process's stat line is shorter than the processor-time fields")
	}
	utime, err := strconv.ParseUint(fields[utimeIndex], 10, 64)
	if err != nil {
		return usage{}, fmt.Errorf("parse the process's user time: %w", err)
	}
	stime, err := strconv.ParseUint(fields[stimeIndex], 10, 64)
	if err != nil {
		return usage{}, fmt.Errorf("parse the process's system time: %w", err)
	}

	status, err := os.ReadFile(fmt.Sprintf("/proc/%d/status", pid))
	if err != nil {
		return usage{}, fmt.Errorf("read the process's resident size: %w", err)
	}
	var rss uint64
	for line := range strings.Lines(string(status)) {
		if !strings.HasPrefix(line, "VmRSS:") {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) < 2 {
			break
		}
		kib, err := strconv.ParseUint(fields[1], 10, 64)
		if err != nil {
			return usage{}, fmt.Errorf("parse the process's resident size: %w", err)
		}
		rss = kib << 10
	}
	return usage{
		cpuSeconds: float64(utime+stime) / clockTicksPerSecond,
		rssBytes:   rss,
	}, nil
}
