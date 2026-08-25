/* In-tree test cdylib matching coil-tls `native/tls.h` (COI-208).
 * Seven coil_tls_* symbols plus stub-only hooks so machine tests can force
 * WouldBlock, including enable returning WouldBlock with a live session.
 *
 * err_out is a `const char **` (NULL = success). coil_tls_disable is
 * close_notify only; coil_tls_free is the destructor. Tests assert the VM
 * wrapper never treats disable as free. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define ERR_WOULD_BLOCK 0
#define ERR_HANDSHAKE 11

typedef struct {
    int32_t read_calls;
    int32_t write_calls;
    int32_t disable_calls;
    int32_t would_block_reads;
    int32_t would_block_writes;
    uint8_t payload[256];
    uint32_t payload_len;
    uint32_t payload_pos;
    uint8_t last_write[256];
    uint32_t last_write_len;
    uint8_t alpn[32];
    uint32_t alpn_len;
} StubSession;

static int32_t g_next_enable_err = -1; /* -1 = success (NULL err_out) */
static int32_t g_live_sessions = 0;
static int32_t g_enable_calls = 0;
static int32_t g_free_calls = 0;
static int32_t g_disable_calls = 0;

static const char *tag_name(int32_t err) {
    switch (err) {
    case 0:
        return "WouldBlock";
    case 1:
        return "NotFound";
    case 2:
        return "PermissionDenied";
    case 3:
        return "AlreadyClosed";
    case 4:
        return "InvalidInput";
    case 5:
        return "Other";
    case 6:
        return "NotADirectory";
    case 7:
        return "AlreadyExists";
    case 8:
        return "TimedOut";
    case 9:
        return "Truncated";
    case 10:
        return "Certificate";
    case 11:
        return "Handshake";
    default:
        return NULL;
    }
}

static StubSession *as_session(int64_t p) { return (StubSession *)(intptr_t)p; }

static int32_t take_enable_err(void) {
    int32_t err = g_next_enable_err;
    g_next_enable_err = -1;
    return err;
}

static void write_err(const char **err_out, int32_t err) {
    const char *name = tag_name(err);
    if (err_out) {
        *err_out = name;
    }
}

static int64_t new_session(void) {
    StubSession *s = (StubSession *)calloc(1, sizeof(StubSession));
    if (!s) {
        return 0;
    }
    memcpy(s->payload, "hello", 5);
    s->payload_len = 5;
    memcpy(s->alpn, "h2", 2);
    s->alpn_len = 2;
    g_live_sessions += 1;
    return (int64_t)(intptr_t)s;
}

int64_t coil_tls_client_enable(int64_t fd, const char *host, int64_t verify,
                               const char *ca_pem, const char *ca_path,
                               int64_t timeout_ms, const char *alpn,
                               const char **err_out) {
    (void)fd;
    (void)host;
    (void)verify;
    (void)ca_pem;
    (void)ca_path;
    (void)timeout_ms;
    (void)alpn;
    g_enable_calls += 1;
    int32_t err = take_enable_err();
    write_err(err_out, err);
    if (err != -1 && err != ERR_WOULD_BLOCK) {
        return 0;
    }
    return new_session();
}

int64_t coil_tls_server_enable(int64_t fd, const char *cert_pem, const char *key_pem,
                               int64_t timeout_ms, const char *client_ca_pem,
                               const char *alpn, const char **err_out) {
    (void)fd;
    (void)cert_pem;
    (void)key_pem;
    (void)timeout_ms;
    (void)client_ca_pem;
    (void)alpn;
    g_enable_calls += 1;
    int32_t err = take_enable_err();
    write_err(err_out, err);
    if (err != -1 && err != ERR_WOULD_BLOCK) {
        return 0;
    }
    return new_session();
}

int64_t coil_tls_read(int64_t session, int64_t fd, uint8_t *buf, int64_t len,
                      const char **err_out) {
    (void)fd;
    StubSession *s = as_session(session);
    if (!s) {
        write_err(err_out, 5);
        return -1;
    }
    s->read_calls += 1;
    if (s->would_block_reads > 0) {
        s->would_block_reads -= 1;
        write_err(err_out, ERR_WOULD_BLOCK);
        return -1;
    }
    write_err(err_out, -1);
    if (len <= 0) {
        return 0;
    }
    if (s->payload_pos >= s->payload_len) {
        return 0;
    }
    int64_t avail = (int64_t)(s->payload_len - s->payload_pos);
    int64_t n = avail < len ? avail : len;
    if (buf && n > 0) {
        memcpy(buf, s->payload + s->payload_pos, (size_t)n);
    }
    s->payload_pos += (uint32_t)n;
    return n;
}

int64_t coil_tls_write(int64_t session, int64_t fd, const uint8_t *buf,
                       int64_t len, const char **err_out) {
    (void)fd;
    StubSession *s = as_session(session);
    if (!s) {
        write_err(err_out, 5);
        return -1;
    }
    s->write_calls += 1;
    if (s->would_block_writes > 0) {
        s->would_block_writes -= 1;
        write_err(err_out, ERR_WOULD_BLOCK);
        return -1;
    }
    write_err(err_out, -1);
    int64_t n = len;
    if (n < 0) {
        n = 0;
    }
    if (n > 256) {
        n = 256;
    }
    if (buf && n > 0) {
        memcpy(s->last_write, buf, (size_t)n);
    }
    s->last_write_len = (uint32_t)n;
    return n;
}

int64_t coil_tls_alpn(int64_t session, uint8_t *out, int64_t out_len) {
    StubSession *s = as_session(session);
    if (!s) {
        return -1;
    }
    int64_t n = (int64_t)s->alpn_len;
    if (out_len < 0) {
        return -1;
    }
    if (out == NULL || out_len == 0) {
        return n;
    }
    if (n > out_len) {
        n = out_len;
    }
    if (out && n > 0) {
        memcpy(out, s->alpn, (size_t)n);
    }
    return n;
}

void coil_tls_disable(int64_t session, int64_t fd, const char **err_out) {
    (void)fd;
    StubSession *s = as_session(session);
    g_disable_calls += 1;
    if (s) {
        s->disable_calls += 1;
    }
    write_err(err_out, -1);
}

void coil_tls_free(int64_t session) {
    g_free_calls += 1;
    if (session) {
        g_live_sessions -= 1;
        free((void *)(intptr_t)session);
    }
}

void coil_tls_stub_set_would_block_reads(int64_t session, int32_t n) {
    StubSession *s = as_session(session);
    if (s) {
        s->would_block_reads = n;
    }
}

void coil_tls_stub_set_would_block_writes(int64_t session, int32_t n) {
    StubSession *s = as_session(session);
    if (s) {
        s->would_block_writes = n;
    }
}

int32_t coil_tls_stub_read_calls(int64_t session) {
    StubSession *s = as_session(session);
    return s ? s->read_calls : -1;
}

int32_t coil_tls_stub_write_calls(int64_t session) {
    StubSession *s = as_session(session);
    return s ? s->write_calls : -1;
}

int32_t coil_tls_stub_disable_calls(int64_t session) {
    StubSession *s = as_session(session);
    return s ? s->disable_calls : -1;
}

/* One-shot `err_out` for the next client/server enable. WouldBlock still
 * returns a live session; other errors return 0. */
void coil_tls_stub_set_next_enable_err(int32_t err) { g_next_enable_err = err; }

int32_t coil_tls_stub_live_sessions(void) { return g_live_sessions; }

int32_t coil_tls_stub_enable_calls(void) { return g_enable_calls; }

int32_t coil_tls_stub_free_calls(void) { return g_free_calls; }

int32_t coil_tls_stub_disable_calls_total(void) { return g_disable_calls; }
