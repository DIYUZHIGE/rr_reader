idf_component_register(
    SRCS "rr_idf_shim.c" "tjpgd.c"
    INCLUDE_DIRS "include"
    REQUIRES esp_driver_sdspi
)
