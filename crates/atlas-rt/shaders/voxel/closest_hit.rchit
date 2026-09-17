#version 460

#extension GL_EXT_scalar_block_layout : enable
#extension GL_EXT_GOOGLE_include_directive : enable
#extension GL_EXT_shader_explicit_arithmetic_types_int64 : require
#extension GL_EXT_buffer_reference2 : require

#include "../common/header.glsl"
#include "../common/bindings.glsl"

layout(location = 0) rayPayloadInEXT MainPassPayload incoming_payload;
layout(location = 1) rayPayloadInEXT MainPassPayload shadow_payload;

void main() {
    // Palette entries are sRGB; the display path (ACES + gamma) is linear.
    incoming_payload.color = vec4(pow(palette.colors[gl_HitKindEXT].rgb, vec3(2.2)), 1.0);
    incoming_payload.t = gl_RayTmaxEXT;

    vec3 hit_point = gl_ObjectRayOriginEXT + gl_ObjectRayDirectionEXT * gl_RayTmaxEXT;
    int face = -1;

    for (int a = 0; a < 3; ++a) {
        if (gl_ObjectRayDirectionEXT[a] == 0.0) {
            continue;
        }

        float f = hit_point[a] - floor(hit_point[a]);
        // intersect.rint snaps t to the entered face using division. Computing
        // p = o + d*t adds multiply/add error to the division's residual, leaving
        // p within a dozen ULP of the boundary at this coordinate scale.
        // Object space limits |p| to the region edge. With a 32 ULP tolerance,
        // the false-positive window on non-crossed axes remains a fraction of a
        // percent of a cell.
        float eps = 32.0 * 1.1920929e-07 * max(abs(hit_point[a]), 1.0);

        if (f < eps || f > 1.0 - eps) {
            face = a;
            break;
        }
    }

    vec3 normal = -normalize(gl_ObjectRayDirectionEXT);

    if (face >= 0) {
        normal = vec3(0.0);
        normal[face] = -sign(gl_ObjectRayDirectionEXT[face]);
    }

    incoming_payload.normal = normal;
}
