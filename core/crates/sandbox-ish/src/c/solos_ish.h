/* solos_ish.h — the whole surface Rust needs from the iSH kernel.
 *
 * The kernel's own headers are macro-heavy and marked ISH_INTERNAL, so
 * rather than bind them wholesale this shim exposes the handful of calls the
 * sandbox actually makes. Everything here is plain C with no Foundation
 * dependency, so the crate builds for any Apple target with `cc`.
 */
#ifndef SOLOS_ISH_H
#define SOLOS_ISH_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Handles for one spawned guest process. The fds are host fds owned by the
 * caller, who must close them. */
typedef struct {
    int pid;
    int stdin_write;
    int stdout_read;
    int stderr_read;
} solos_ish_proc;

/* Negative return values. */
#define SOLOS_ISH_OK              0
#define SOLOS_ISH_ERR_BOOT       -1
#define SOLOS_ISH_ERR_NOT_BOOTED -2
#define SOLOS_ISH_ERR_PIPE       -3
#define SOLOS_ISH_ERR_SPAWN      -4
#define SOLOS_ISH_ERR_EXEC       -5
#define SOLOS_ISH_ERR_ARGS       -6

/* Boot the kernel on a fakefs rootfs (a directory holding data/ and meta.db).
 * Idempotent; safe to call from any thread. */
int solos_ish_boot(const char *rootfs_dir);
int solos_ish_is_booted(void);

/* Redirect a guest path to a host directory. meta.db entries are created by
 * fakefs on access, so the host directory is the only prerequisite. */
int solos_ish_bind_mount(const char *guest_path, const char *host_path, int read_only);
int solos_ish_bind_unmount(const char *guest_path);

/* Start a guest process with its stdio on pipes. `argv` and `envp` are
 * NULL-terminated arrays of NUL-terminated strings. Safe to call from any
 * thread: the kernel's notion of the current task is thread-local, so the
 * call is serialised and the caller's value restored before returning. */
int solos_ish_spawn(const char *path,
                   const char *const *argv,
                   const char *const *envp,
                   solos_ish_proc *out);

/* ------------------------------------------------------------------ */
/* Pseudo-terminal                                                      */
/* ------------------------------------------------------------------ */

/* A guest process on a real pty: line discipline, job control, window size,
 * and Ctrl-C all behave, because the terminal is the guest kernel's own
 * (fs/pty.c) rather than something faked on the host. The host plays the
 * terminal: it receives what the program prints through the output callback
 * and sends keystrokes back with solos_ish_pty_input.
 *
 * `pty_id` identifies the terminal in every later call. */
typedef struct {
    int pid;
    int pty_id;
} solos_ish_pty;

/* Called on a guest thread whenever the program writes to its terminal. Must
 * not block, and must not call back into the guest. */
typedef void (*solos_ish_pty_out_cb)(int pty_id, const char *buf, int len, void *ctx);
void solos_ish_set_pty_handler(solos_ish_pty_out_cb cb, void *ctx);

/* Start a program with a pty for its stdin, stdout and stderr. The process
 * becomes a session leader and the pty becomes its controlling terminal, so
 * Ctrl-C reaches it as SIGINT. */
int solos_ish_spawn_pty(const char *path,
                       const char *const *argv,
                       const char *const *envp,
                       int rows,
                       int cols,
                       solos_ish_pty *out);

/* Keystrokes from the user into the terminal. Returns bytes accepted. */
int solos_ish_pty_input(int pty_id, const char *buf, int len);

/* Tell the program its window changed; sends SIGWINCH. */
int solos_ish_pty_resize(int pty_id, int rows, int cols);

/* Hang up: the program sees its terminal disappear, as closing one does. */
int solos_ish_pty_close(int pty_id);

/* SIGKILL the process group led by `pid`, plus its descendants. Mirrors what
 * a host `kill -9 -pgid` would reach. */
int solos_ish_kill_group(int pid);

/* Stop or resume every guest process except init. This is the CPU governor
 * (spec §6.1): iOS kills an app that burns processor in the background, and a
 * stopped guest burns none. `stop` non-zero sends SIGSTOP, zero sends
 * SIGCONT. Returns how many processes were signalled. */
int solos_ish_signal_all(int stop);

/* Called on a kernel thread when a process exits; `code` is the wait status.
 * Must not block. Replaces any previous handler. */
typedef void (*solos_ish_exit_cb)(int pid, int code, void *ctx);
void solos_ish_set_exit_handler(solos_ish_exit_cb cb, void *ctx);

/* Rewrite /etc/resolv.conf inside the guest. Pass a newline-separated list of
 * nameserver addresses; the guest sees one `nameserver` line per entry.
 *
 * Pass NULL or an empty string to copy the host's own resolver configuration
 * (its nameservers and search domains) instead, which is what the guest wants
 * on a phone: the network path changes under it and 8.8.8.8 is not always
 * reachable. Falls back to public resolvers when the host reports none.
 *
 * Cheap to call repeatedly: the guest file is only rewritten when the text
 * it would contain has changed. Returns SOLOS_ISH_OK when the guest is in
 * sync, whether or not anything was written. */
int solos_ish_set_dns(const char *nameservers);

/* Write a file inside the guest, creating or truncating it.
 *
 * Goes through the guest's own filesystem rather than poking at `data/`
 * behind fakefs's back, so the metadata database stays consistent. Parent
 * directories must already exist.
 *
 * Exists so Solos can lay down its own `/etc/profile` after boot: the rootfs
 * image carries someone else's, and the last thing that file does decides
 * where every shell starts. */
int solos_ish_write_file(const char *path, const char *content, int length, int mode);

#ifdef __cplusplus
}
#endif
#endif /* SOLOS_ISH_H */
