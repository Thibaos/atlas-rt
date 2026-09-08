shader_type canvas_item;

uniform sampler2D atlas_frame;
uniform int mode = 0;

vec3 aces(vec3 color) {
    return clamp((color * (2.51 * color + 0.03)) / (color * (2.43 * color + 0.59) + 0.14), vec3(0.0), vec3(1.0));
}

float dither(vec2 pixel) {
    return fract(sin(dot(pixel, vec2(12.9898, 78.233))) * 43758.5453) / 255.0;
}

void fragment() {
    vec3 linear = texture(atlas_frame, UV).rgb;

    vec3 final_data;

    if (mode == 0) {
        vec3 tone_mapped = aces(linear);
        final_data = pow(tone_mapped, vec3(1.0 / 2.2));
    } else {
        final_data = linear;
    }

    float jitter = dither(SCREEN_PIXEL_SIZE * SCREEN_UV) - 0.5 / 255.0;
    COLOR = vec4(final_data + jitter, 1.0);
}
