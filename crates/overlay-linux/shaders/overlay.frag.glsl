#version 450
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 out_color;
layout(set = 0, binding = 0) uniform texture2D overlay_tex;
layout(set = 0, binding = 1) uniform sampler overlay_sampler;
void main() {
    // B8G8R8A8_UNORM sampling exposes BGRA bytes as semantic RGBA.
    // Premultiplied CEF color composes with ONE / ONE_MINUS_SRC_ALPHA.
    out_color = texture(sampler2D(overlay_tex, overlay_sampler), uv);
}
