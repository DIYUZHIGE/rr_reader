#pragma once

#if __has_include("driver/sdspi_host.h")
#include "driver/sdspi_host.h"
#else
typedef int spi_host_device_t;
typedef struct {
    int flags;
    int slot;
    int max_freq_khz;
    float io_voltage;
    void* command_timeout_ms;
    int get_bus_width;
    int set_card_clk;
    int init;
    int deinit;
    int io_int_enable;
    int io_int_wait;
    int command;
    int set_bus_width;
    int get_bus_width2;
    int set_bus_ddr_mode;
    int set_cclk_always_on;
    int do_transaction;
    int __unused;
} sdmmc_host_t;
#endif
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
