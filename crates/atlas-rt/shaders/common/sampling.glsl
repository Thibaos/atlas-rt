const vec2 R2_STEP = vec2(0.7548777, 0.5698403);

const uint HASH_FIELD_BITS = 16u;
const uint HASH_FIELD_MASK = 0xFFFFu;
const float HASH_FIELD_RANGE = 65536.0;

uint hash_mix(uint x) {
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

vec2 r2_point(uint i) {
    return fract(0.5 + float(i) * R2_STEP);
}

vec2 pixel_shift(uvec2 pixel) {
    uint hash = hash_mix(pixel.x ^ (pixel.y << HASH_FIELD_BITS));

    return vec2(float(hash & HASH_FIELD_MASK), float(hash >> HASH_FIELD_BITS)) / HASH_FIELD_RANGE;
}

void orthonormal_basis(vec3 normal, out vec3 tangent, out vec3 bitangent) {
    float z_sign = (normal.z >= 0.0) ? 1.0 : -1.0;
    float a = -1.0 / (z_sign + normal.z);
    float b = normal.x * normal.y * a;

    tangent = vec3(1.0 + z_sign * normal.x * normal.x * a, z_sign * b, -z_sign * normal.x);
    bitangent = vec3(b, z_sign + normal.y * normal.y * a, -normal.y);
}
