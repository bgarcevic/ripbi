/* ripbi's shim over the XPress9 public API. Unlike the upstream wrapper it
 * never prints: every failure is returned to Rust as a status code. */
#include <stdlib.h>
#include "xpress.h"
#include "xpress9.h"

#define RIPBI_OK 0u
#define RIPBI_OUTPUT_OVERFLOW 1000u

static void *XPRESS_CALL ripbi_alloc(void *context, int size)
{
    (void)context;
    return size > 0 ? malloc((size_t)size) : NULL;
}

static void XPRESS_CALL ripbi_free(void *context, void *address)
{
    (void)context;
    free(address);
}

/* Creates a decoder with an open session; NULL on failure. */
void *ripbi_xpress9_decoder_new(void)
{
    XPRESS9_STATUS status = {0};
    XPRESS9_DECODER decoder = Xpress9DecoderCreate(
        &status, NULL, ripbi_alloc, XPRESS9_WINDOW_SIZE_LOG2_MAX, 0);
    if (decoder == NULL || status.m_uStatus != Xpress9Status_OK) {
        return NULL;
    }
    Xpress9DecoderStartSession(&status, decoder, 1);
    if (status.m_uStatus != Xpress9Status_OK) {
        XPRESS9_STATUS ignored = {0};
        Xpress9DecoderDestroy(&ignored, decoder, NULL, ripbi_free);
        return NULL;
    }
    return (void *)decoder;
}

void ripbi_xpress9_decoder_free(void *decoder)
{
    if (decoder != NULL) {
        XPRESS9_STATUS status = {0};
        Xpress9DecoderDestroy(&status, (XPRESS9_DECODER)decoder, NULL, ripbi_free);
    }
}

/* Decodes one chunk into `out`, keeping the decoder's history window for the
 * next chunk. `*written` receives the byte count; the return value is an
 * Xpress9Status_* code, or RIPBI_OUTPUT_OVERFLOW when the chunk decodes to
 * more than `out_len` bytes. */
unsigned ripbi_xpress9_decode(void *decoder, const unsigned char *in, unsigned in_len,
                              unsigned char *out, unsigned out_len, unsigned *written)
{
    XPRESS9_STATUS status = {0};
    XPRESS9_STATUS detach = {0};
    XPRESS9_DECODER handle = (XPRESS9_DECODER)decoder;
    unsigned result = RIPBI_OK;
    *written = 0;
    Xpress9DecoderAttach(&status, handle, in, in_len);
    if (status.m_uStatus != Xpress9Status_OK) {
        return status.m_uStatus;
    }
    for (;;) {
        unsigned chunk = 0;
        unsigned needed = 0;
        unsigned remaining = Xpress9DecoderFetchDecompressedData(
            &status, handle, out + *written, out_len - *written, &chunk, &needed);
        if (status.m_uStatus != Xpress9Status_OK) {
            result = status.m_uStatus;
            break;
        }
        *written += chunk;
        if (remaining == 0) {
            break;
        }
        if (chunk == 0 || *written == out_len) {
            result = RIPBI_OUTPUT_OVERFLOW;
            break;
        }
    }
    Xpress9DecoderDetach(&detach, handle, in, in_len);
    if (result == RIPBI_OK && detach.m_uStatus != Xpress9Status_OK) {
        result = detach.m_uStatus;
    }
    return result;
}

#ifdef RIPBI_XPRESS9_ENCODER
/* Creates an encoder with an open session; NULL on failure. */
void *ripbi_xpress9_encoder_new(void)
{
    XPRESS9_STATUS status = {0};
    XPRESS9_ENCODER_PARAMS params = {0};
    XPRESS9_ENCODER encoder = Xpress9EncoderCreate(
        &status, NULL, ripbi_alloc, XPRESS9_WINDOW_SIZE_LOG2_MAX, 0);
    if (encoder == NULL || status.m_uStatus != Xpress9Status_OK) {
        return NULL;
    }
    params.m_cbSize = sizeof(params);
    params.m_uMaxStreamLength = 0;
    params.m_uMtfEntryCount = 4;
    params.m_uLookupDepth = 9;
    params.m_uOptimizationLevel = 0;
    params.m_uPtrMinMatchLength = 4;
    params.m_uMtfMinMatchLength = 2;
    params.m_uWindowSizeLog2 = XPRESS9_WINDOW_SIZE_LOG2_MAX;
    Xpress9EncoderStartSession(&status, encoder, &params, 1);
    if (status.m_uStatus != Xpress9Status_OK) {
        XPRESS9_STATUS ignored = {0};
        Xpress9EncoderDestroy(&ignored, encoder, NULL, ripbi_free);
        return NULL;
    }
    return (void *)encoder;
}

void ripbi_xpress9_encoder_free(void *encoder)
{
    if (encoder != NULL) {
        XPRESS9_STATUS status = {0};
        Xpress9EncoderDestroy(&status, (XPRESS9_ENCODER)encoder, NULL, ripbi_free);
    }
}

/* Encodes one flushed chunk that shares history with earlier chunks. Returns
 * a status code as ripbi_xpress9_decode does. */
unsigned ripbi_xpress9_encode(void *encoder, const unsigned char *in, unsigned in_len,
                              unsigned char *out, unsigned out_len, unsigned *written)
{
    XPRESS9_STATUS status = {0};
    XPRESS9_STATUS detach = {0};
    XPRESS9_ENCODER handle = (XPRESS9_ENCODER)encoder;
    unsigned result = RIPBI_OK;
    *written = 0;
    Xpress9EncoderAttach(&status, handle, in, in_len, 1);
    if (status.m_uStatus != Xpress9Status_OK) {
        return status.m_uStatus;
    }
    for (;;) {
        unsigned promised = Xpress9EncoderCompress(&status, handle, NULL, NULL);
        if (status.m_uStatus != Xpress9Status_OK) {
            result = status.m_uStatus;
            break;
        }
        if (promised == 0) {
            break;
        }
        if (promised > out_len - *written) {
            result = RIPBI_OUTPUT_OVERFLOW;
            break;
        }
        for (;;) {
            unsigned fetched = 0;
            unsigned more = Xpress9EncoderFetchCompressedData(
                &status, handle, out + *written, out_len - *written, &fetched);
            if (status.m_uStatus != Xpress9Status_OK) {
                result = status.m_uStatus;
                break;
            }
            *written += fetched;
            if (more == 0) {
                break;
            }
        }
        if (result != RIPBI_OK) {
            break;
        }
    }
    Xpress9EncoderDetach(&detach, handle, in, in_len);
    return result;
}
#endif
