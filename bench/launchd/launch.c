// ctadl-bench-launch: run a command as a child with stdout/stderr appended to a log file.
//
//   ctadl-bench-launch <log> <command> [args...]
//
// It exists for macOS privacy controls (TCC). Jobs launchd starts may not touch an external
// volume such as /Volumes/Shampoo -- launchd cannot even open a log file there -- unless the
// program it starts has Full Disk Access. Granting that to /bin/sh or a Python interpreter in
// the Nix store would be too broad or would not survive an update, so this small, stable
// binary is what gets the grant. It spawns the command (rather than exec'ing it), so the
// command and everything under it stay attributed to this launcher; it forwards termination
// signals to the child and exits with its status.

#include <fcntl.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
static pid_t child;

static void forward(int sig) {
    if (child > 0) kill(child, sig);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <log> <command> [args...]\n", argv[0]);
        return 2;
    }
    int fd = open(argv[1], O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd < 0) {
        perror(argv[1]);
        return 78;
    }
    dup2(fd, STDOUT_FILENO);
    dup2(fd, STDERR_FILENO);
    close(fd);

    struct sigaction sa = {0};
    sa.sa_handler = forward;
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    sigaction(SIGHUP, &sa, NULL);

    int err = posix_spawn(&child, argv[2], NULL, NULL, argv + 2, environ);
    if (err) {
        fprintf(stderr, "spawn %s: error %d\n", argv[2], err);
        return 127;
    }
    int status;
    while (waitpid(child, &status, 0) < 0) {
    }
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    return 128 + WTERMSIG(status);
}
