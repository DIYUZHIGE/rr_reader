#pragma once

#include "driver/sdspi_host.h"
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int (*rr_jpeg_gray_block_cb)(
    void* ctx,
    const uint8_t* gray,
    uint16_t left,
    uint16_t top,
    uint16_t right,
    uint16_t bottom
);

sdmmc_host_t rr_sdspi_host_default(spi_host_device_t host_id);

int rr_decode_jpeg_streaming(
    const char* path,
    uint8_t scale,
    rr_jpeg_gray_block_cb cb,
    void* ctx,
    uint16_t* out_width,
    uint16_t* out_height
);

#ifdef __cplusplus
}
#endif
