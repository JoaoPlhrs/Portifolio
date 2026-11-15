/* A versão de ABI do libvpx muda entre versões da biblioteca. Fixá-la no Rust
 * daria erro silencioso em outro sistema, então ela é lida do cabeçalho na
 * hora de compilar. */
#include <vpx/vpx_encoder.h>
#include <vpx/vp8cx.h>

int jm_vpx_encoder_abi_version(void) { return VPX_ENCODER_ABI_VERSION; }
