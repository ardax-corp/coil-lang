/* In-tree test cdylib for COI-208. Implements the seven coil_tls_* symbols
 * plus a few stub-only hooks so machine tests can force WouldBlock. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define ABI_OK ((int32_t)-1)
#define ERR_WOULD_BLOCK ((int32_t)0)

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

static StubSession *as_session(void *p) { return (StubSession *)p; }

static void *new_session(void) {
    StubSession *s = (StubSession *)calloc(1, sizeof(StubSession));
    if (!s) {
        return NULL;
    }
    memcpy(s->payload, "hello", 5);
    s->payload_len = 5;
    memcpy(s->alpn, "h2", 2);
    s->alpn_len = 2;
    return s;
}

void *coil_tls_client_enable(int64_t fd, const char *host, int32_t verify,
                             const char *ca_pem, const char *ca_path,
                             int64_t timeout_ms, const char *alpn,
                             int32_t *err_out) {
    (void)fd;
    (void)host;
    (void)verify;
    (void)ca_pem;
    (void)ca_path;
    (void)timeout_ms;
    (void)alpn;
    if (err_out) {
        *err_out = ABI_OK;
    }
    return new_session();
}

void *coil_tls_server_enable(int64_t fd, const char *cert_pem, const char *key_pem,
                             int64_t timeout_ms, const char *client_ca_pem,
                             const char *alpn, int32_t *err_out) {
    (void)fd;
    (void)cert_pem;
    (void)key_pem;
    (void)timeout_ms;
    (void)client_ca_pem;
    (void)alpn;
    if (err_out) {
        *err_out = ABI_OK;
    }
    return new_session();
}

intptr_t coil_tls_read(void *session, int64_t fd, uint8_t *buf, uintptr_t len,
                       int32_t *err_out) {
    (void)fd;
    StubSession *s = as_session(session);
    if (!s || !err_out) {
        if (err_out) {
            *err_out = 5; /* Other */
        }
        return -1;
    }
    s->read_calls += 1;
    if (s->would_block_reads > 0) {
        s->would_block_reads -= 1;
        *err_out = ERR_WOULD_BLOCK;
        return -1;
    }
    *err_out = ABI_OK;
    if (s->payload_pos >= s->payload_len) {
        return 0;
    }
    uintptr_t avail = (uintptr_t)(s->payload_len - s->payload_pos);
    uintptr_t n = avail < len ? avail : len;
    if (buf && n > 0) {
        memcpy(buf, s->payload + s->payload_pos, (size_t)n);
    }
    s->payload_pos += (uint32_t)n;
    return (intptr_t)n;
}

intptr_t coil_tls_write(void *session, int64_t fd, const uint8_t *buf,
                        uintptr_t len, int32_t *err_out) {
    (void)fd;
    StubSession *s = as_session(session);
    if (!s || !err_out) {
        if (err_out) {
            *err_out = 5;
        }
        return -1;
    }
    s->write_calls += 1;
    if (s->would_block_writes > 0) {
        s->would_block_writes -= 1;
        *err_out = ERR_WOULD_BLOCK;
        return -1;
    }
    *err_out = ABI_OK;
    uintptr_t n = len;
    if (n > 256) {
        n = 256;
    }
    if (buf && n > 0) {
        memcpy(s->last_write, buf, (size_t)n);
    }
    s->last_write_len = (uint32_t)n;
    return (intptr_t)n;
}

intptr_t coil_tls_alpn(void *session, uint8_t *out, uintptr_t out_len) {
    StubSession *s = as_session(session);
    if (!s) {
        return 0;
    }
    uintptr_t n = (uintptr_t)s->alpn_len;
    if (n > out_len) {
        n = out_len;
    }
    if (out && n > 0) {
        memcpy(out, s->alpn, (size_t)n);
    }
    return (intptr_t)n;
}

int32_t coil_tls_disable(void *session, int64_t fd, int32_t *err_out) {
    (void)fd;
    StubSession *s = as_session(session);
    if (s) {
        s->disable_calls += 1;
        /* Snapshot counts are lost after free; tests read them before disable. */
        free(s);
    }
    if (err_out) {
        *err_out = ABI_OK;
    }
    return 0;
}

void coil_tls_free(void *session) { free(session); }

void coil_tls_stub_set_would_block_reads(void *session, int32_t n) {
    StubSession *s = as_session(session);
    if (s) {
        s->would_block_reads = n;
    }
}

void coil_tls_stub_set_would_block_writes(void *session, int32_t n) {
    StubSession *s = as_session(session);
    if (s) {
        s->would_block_writes = n;
    }
}

int32_t coil_tls_stub_read_calls(void *session) {
    StubSession *s = as_session(session);
    return s ? s->read_calls : -1;
}

int32_t coil_tls_stub_write_calls(void *session) {
    StubSession *s = as_session(session);
    return s ? s->write_calls : -1;
}

int32_t coil_tls_stub_disable_calls(void *session) {
    StubSession *s = as_session(session);
    return s ? s->disable_calls : -1;
}
