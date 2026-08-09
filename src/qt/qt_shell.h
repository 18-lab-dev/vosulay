#pragma once

#include "rust/cxx.h"

#include <cstdint>

namespace vosulay {

struct QtShellOptions;

std::int32_t run_qt_application(rust::Str arguments_json,
                                const QtShellOptions &options);

} // namespace vosulay
