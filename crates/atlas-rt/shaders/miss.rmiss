#version 460

#extension GL_EXT_scalar_block_layout : enable
#extension GL_EXT_GOOGLE_include_directive : enable
#extension GL_EXT_shader_explicit_arithmetic_types_int64 : require
#extension GL_EXT_buffer_reference2 : require

#include "common/header.glsl"
#include "common/bindings.glsl"
#include "common/sky.glsl"

layout(location = 0) rayPayloadInEXT MainPassPayload incoming_payload;
layout(location = 1) rayPayloadInEXT MainPassPayload shadow_payload;
layout(location = 2) rayPayloadInEXT OcclusionPayload occlusion;

void main() {
    vec3 sky = sky_radiance(gl_WorldRayDirectionEXT);

    incoming_payload.color = vec4(sky, 1.0);
    incoming_payload.alpha = 1.0;
    incoming_payload.normal = vec3(0.0);
    incoming_payload.t = FLT_MAX;

    shadow_payload.t = FLT_MAX;

    occlusion = OcclusionPayload(sky, 1.0);
}
