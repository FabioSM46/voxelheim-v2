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
	"time"

	"github.com/FabioSM46/voxelheim-v2/server/internal/session"
)

// The server under test, started here as cmd/voxelheim-voicebot starts it (copied, since a
// command cannot import another): an ephemeral world on a kernel-chosen port, its address
// and fingerprint read from its Info startup lines, and a SIGKILL at the end.

const (
	listeningMessage   = "voxelheimd listening"
	certificateMessage = "listening with an encrypted session"
	serverStartLimit   = 60 * time.Second
)

type serverProcess struct {
	cmd         *exec.Cmd
	commandLine string
	addr        string
	fingerprint string

	mu   sync.Mutex
	tail []string
	done sync.WaitGroup
}

func serverArgs(o options, ticketKey string) []string {
	return []string{
		"-listen", "127.0.0.1:0",
		"-world-dir=",
		"-world-name", o.worldName,
		"-ticket-key", ticketKey,
		"-seed", strconv.FormatInt(o.seed, 10),
		"-view-distance", strconv.Itoa(o.viewDistance),
		"-max-players", strconv.Itoa(session.MinConcurrentSessions),
		// /teleport and /additem. See route.go for exactly what each is used
		// for; none of them opens a door, lights a rune or kills anything.
		"-dev-commands",
		"-log-format", "json",
		"-log-level", "info",
	}
}

func startServer(ctx context.Context, o options, ticketKey string) (*serverProcess, error) {
	args := serverArgs(o, ticketKey)
	cmd := exec.CommandContext(ctx, o.serverBin, args...) //nolint:gosec // the binary is the operator's own flag.
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("open the server's log: %w", err)
	}
	cmd.Stdout = io.Discard
	cmd.Cancel = func() error { return cmd.Process.Kill() }
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start %s: %w", o.serverBin, err)
	}
	s := &serverProcess{cmd: cmd, commandLine: strings.Join(append([]string{"voxelheimd"}, args...), " ")}
	ready := make(chan error, 1)
	s.done.Add(1)
	go s.scan(stderr, ready)

	timer := time.NewTimer(serverStartLimit)
	defer timer.Stop()
	select {
	case err := <-ready:
		if err != nil {
			_ = cmd.Process.Kill()
			return nil, err
		}
		return s, nil
	case <-timer.C:
		_ = cmd.Process.Kill()
		return nil, fmt.Errorf("the server did not say where it listens within %v:\n%s", serverStartLimit, s.lastLines())
	case <-ctx.Done():
		_ = cmd.Process.Kill()
		return nil, ctx.Err()
	}
}

func (s *serverProcess) scan(stderr io.Reader, ready chan<- error) {
	defer s.done.Done()
	scanner := bufio.NewScanner(stderr)
	scanner.Buffer(make([]byte, 0, 64<<10), 1<<20)
	var haveAddr, haveFingerprint, announced bool
	for scanner.Scan() {
		line := scanner.Text()
		s.mu.Lock()
		s.tail = append(s.tail, line)
		if len(s.tail) > 40 {
			s.tail = s.tail[1:]
		}
		s.mu.Unlock()
		var record struct {
			Msg         string `json:"msg"`
			Addr        string `json:"addr"`
			Fingerprint string `json:"certificate_sha256"`
		}
		if json.Unmarshal([]byte(line), &record) != nil {
			continue
		}
		switch record.Msg {
		case listeningMessage:
			s.addr, haveAddr = record.Addr, true
		case certificateMessage:
			s.fingerprint, haveFingerprint = record.Fingerprint, true
		}
		if haveAddr && haveFingerprint && !announced {
			announced = true
			ready <- nil
		}
	}
	if !announced {
		ready <- fmt.Errorf("the server stopped before it was listening:\n%s", s.lastLines())
	}
}

func (s *serverProcess) lastLines() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return strings.Join(s.tail, "\n")
}

func (s *serverProcess) stop() {
	if s.cmd.Process != nil {
		_ = s.cmd.Process.Kill()
	}
	_ = s.cmd.Wait()
	s.done.Wait()
}

// residentBytes is the server's resident set, read from /proc. Linux only, and it says so
// by failing rather than by a build tag, so the file stays inside vet and the 32-bit builds.
func residentBytes(pid int) (uint64, error) {
	status, err := os.ReadFile(fmt.Sprintf("/proc/%d/status", pid))
	if err != nil {
		return 0, fmt.Errorf("read the server's resident size: %w", err)
	}
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
			return 0, fmt.Errorf("parse the server's resident size: %w", err)
		}
		return kib << 10, nil
	}
	return 0, errors.New("the server's status has no VmRSS line")
}
