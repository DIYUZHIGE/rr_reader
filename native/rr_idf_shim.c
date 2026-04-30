#include "rr_idf_shim.h"

sdmmc_host_t rr_sdspi_host_default(spi_host_device_t host_id) {
    sdmmc_host_t host = SDSPI_HOST_DEFAULT();
    host.slot = host_id;
    return host;
}
