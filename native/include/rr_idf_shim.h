#pragma once

#include "driver/sdspi_host.h"

#ifdef __cplusplus
extern "C" {
#endif

sdmmc_host_t rr_sdspi_host_default(spi_host_device_t host_id);

#ifdef __cplusplus
}
#endif
