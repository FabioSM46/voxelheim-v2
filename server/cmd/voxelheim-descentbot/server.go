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
// command cannot import another): a temporary durable world on a kernel-chosen port,
// its address and fingerprint read from its Info startup lines. A clean stop waits for
// the server's persistence flush, so restarting that directory tests real saved state.

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

	mu           sync.Mutex
	tail         []string
	done         sync.WaitGroup
	exited       chan struct{}
	waitErr      error // written before exited closes
	shutdownOnce sync.Once
	shutdownErr  error // published by shutdownOnce to every caller
}

func serverArgs(o options, ticketKey string) []string {
	return []string{
		"-listen", "127.0.0.1:0",
		"-world-dir", o.worldDir,
		"-world-name", o.worldName,
		"-ticket-key", ticketKey,
		"-seed", strconv.FormatInt(o.seed, 10),
		"-view-distance", strconv.Itoa(o.viewDistance),
		"-max-players", strconv.Itoa(session.MinConcurrentSessions),
		// /teleport, /additem and optional /addexperience. See route.go for exactly what each is used
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
	s := &serverProcess{cmd: cmd, commandLine: reportServerArgs(o), exited: make(chan struct{})}
	ready := make(chan error, 1)
	s.done.Add(1)
	go s.scan(stderr, ready)
	go func() {
		// Drain the log before Wait closes the pipe; never race the scanner with Wait.
		s.done.Wait()
		s.waitErr = cmd.Wait()
		close(s.exited)
	}()

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

// shutdown is idempotent and refuses to call a forced stop a persistence success.
func (s *serverProcess) shutdown() error {
	s.shutdownOnce.Do(func() { s.shutdownErr = s.shutdownProcess() })
	return s.shutdownErr
}

func (s *serverProcess) shutdownProcess() error {
	select {
	case <-s.exited:
		return s.waitErr
	default:
	}
	if err := s.cmd.Process.Signal(os.Interrupt); err != nil && !errors.Is(err, os.ErrProcessDone) {
		_ = s.cmd.Process.Kill()
		<-s.exited
		return fmt.Errorf("request server shutdown: %w", err)
	}
	timer := time.NewTimer(60 * time.Second)
	defer timer.Stop()
	select {
	case <-s.exited:
		return s.waitErr
	case <-timer.C:
		_ = s.cmd.Process.Kill()
		<-s.exited
		return errors.New("server did not finish its persistence shutdown within one minute")
	}
}

// Reports contain neither the temporary path nor an operational signing key/address.
func reportServerArgs(o options) string {
	return fmt.Sprintf("voxelheimd -world-dir <temporary-world> -world-name %s -seed %d -view-distance %d -dev-commands (ephemeral listener; ticket key omitted)",
		o.worldName, o.seed, o.viewDistance)
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
