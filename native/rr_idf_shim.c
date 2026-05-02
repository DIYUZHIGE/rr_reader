#include "rr_idf_shim.h"
#include "tjpgd.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

sdmmc_host_t rr_sdspi_host_default(spi_host_device_t host_id) {
    sdmmc_host_t host = SDSPI_HOST_DEFAULT();
    host.slot = host_id;
    return host;
}

typedef struct {
    FILE* fp;
    rr_jpeg_gray_block_cb cb;
    void* user_ctx;
    uint16_t max_right;
    uint16_t max_bottom;
} rr_jpeg_ctx_t;

static size_t rr_tjpgd_infunc(JDEC* jd, uint8_t* buff, size_t nbyte) {
    rr_jpeg_ctx_t* ctx = (rr_jpeg_ctx_t*)jd->device;
    if (!ctx || !ctx->fp) {
        return 0;
    }

    if (buff) {
        return fread(buff, 1, nbyte, ctx->fp);
    }

    return (size_t)fseek(ctx->fp, (long)nbyte, SEEK_CUR) == 0 ? nbyte : 0;
}

static int rr_tjpgd_outfunc(JDEC* jd, void* bitmap, JRECT* rect) {
    rr_jpeg_ctx_t* ctx = (rr_jpeg_ctx_t*)jd->device;
    if (!ctx || !ctx->cb || !bitmap || !rect) {
        return 0;
    }

    if (rect->right > ctx->max_right) {
        ctx->max_right = rect->right;
    }
    if (rect->bottom > ctx->max_bottom) {
        ctx->max_bottom = rect->bottom;
    }

    return ctx->cb(
        ctx->user_ctx,
        (const uint8_t*)bitmap,
        rect->left,
        rect->top,
        rect->right,
        rect->bottom
    );
}

int rr_decode_jpeg_streaming(
    const char* path,
    uint8_t scale,
    rr_jpeg_gray_block_cb cb,
    void* ctx,
    uint16_t* out_width,
    uint16_t* out_height
) {
    if (!path || !cb) {
        return JDR_PAR;
    }

    FILE* fp = fopen(path, "rb");
    if (!fp) {
        return JDR_INP;
    }

    JDEC jdec;
    rr_jpeg_ctx_t jpeg_ctx = {
        .fp = fp,
        .cb = cb,
        .user_ctx = ctx,
        .max_right = 0,
        .max_bottom = 0,
    };

    size_t work_size = TJPGD_WORKSPACE_SIZE + 2048;
    void* work = malloc(work_size);
    if (!work) {
        fclose(fp);
        return JDR_MEM1;
    }

    JRESULT prep = jd_prepare(&jdec, rr_tjpgd_infunc, work, work_size, &jpeg_ctx);
    if (prep != JDR_OK) {
        free(work);
        fclose(fp);
        return prep;
    }

    JRESULT rc = jd_decomp(&jdec, rr_tjpgd_outfunc, scale);

    if (out_width) {
        *out_width = (uint16_t)(jpeg_ctx.max_right + 1);
    }
    if (out_height) {
        *out_height = (uint16_t)(jpeg_ctx.max_bottom + 1);
    }

    free(work);
    fclose(fp);
    return rc;
}
