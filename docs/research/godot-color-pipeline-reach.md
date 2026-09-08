# Godot color-pipeline reach for 4.7.2 (Forward+)

Facts for ADR 0006's rejected option "stock Environment tonemap does the color work": whether the
Environment tonemap pass (Linear, Reinhard, Filmic, ACES, AgX) and the adjustments block
(brightness, contrast, saturation, color correction) reach canvas-drawn content in Forward+ 4.7.2,
what rendering/viewport/hdr_2d changes, whether ImageTexture and Texture2DRD differ on this path,
and whether any viewport or project setting routes canvas content through a color-processing pass
before the swapchain.

Short answer, stated loudly. With rendering/viewport/hdr_2d off (the default, and what ADR 0006
pins) and a background that is not BG_CANVAS, stock tonemap and adjustments never touch
canvas-drawn content in Forward+. Two configurations do process canvas content, and both matter:
Environment background_mode BG_CANVAS feeds canvas pixels through the full tonemap pass, and
hdr_2d on switches canvas to linear colors and adds a linear-to-sRGB encode plus optional dither
at present time. On the path atlas-rt uses (full-rect canvas item, hdr_2d off, normal ordering),
the extension's shader is the last color math on its pixels, 8-bit quantization aside.

## Sources and method.

Tag verified two ways: the GitHub refs API resolves refs/tags/4.7.2-stable to commit
ed1daf0bf001b61586d9930840f2f1394092c079 (the same HEAD the earlier fact-find used), and the
tag's version.py reports major 4, minor 7, patch 2, status stable. All line numbers come from a
local extraction of the complete tag source, so no claim is scoped by web_fetch truncation. Files
read in full: servers/rendering/renderer_viewport.cpp, renderer_rd/renderer_compositor_rd.cpp,
renderer_rd/effects/tone_mapper.h, renderer_rd/shaders/blit.glsl, renderer_rd/shaders/effects/
tonemap.glsl, and the fragment side of renderer_rd/shaders/canvas.glsl; targeted sections of
renderer_rd/renderer_scene_render_rd.cpp, renderer_rd/forward_clustered/render_forward_clustered.cpp,
renderer_rd/effects/tone_mapper.cpp, renderer_rd/renderer_canvas_render_rd.cpp, renderer_rd/
storage_rd/texture_storage.cpp, renderer_rd/storage_rd/material_storage.cpp, renderer_rd/storage_rd/
render_scene_buffers_rd.{h,cpp}, scene/main/scene_tree.cpp, scene/main/window.cpp, scene/main/
viewport.cpp, servers/rendering/rendering_server.cpp. Absence claims (the adjustments have no other
consumer, no canvas compositor callback type) rest on greps over the full extracted source.
Scope: Forward+ on Vulkan. The Mobile renderer shares the base dispatch and its subpass variant
(renderer_scene_render_rd.cpp L900-986); Mobile claims are dispatch-level only, gl_compatibility
(GLES) is not traced. Documented behavior is cited from doc/classes XML of the same tag.
Swapchain formats cite godot-rd-external-texture-semantics.md finding 3. GitHub blob URLs take the
form https://github.com/godotengine/godot/blob/4.7.2-stable/<path>#L<first>.

## Findings

1. **The tonemap pass is the last step of the 3D render, canvas draws after it, and canvas pixels
never feed it.** The viewport draw is ordered in RendererViewport::_draw_viewport
(servers/rendering/renderer_viewport.cpp L338-765): can_draw_3d requires a camera and 3D enabled
(L385); unless the Environment background is BG_CANVAS, _draw_3d runs first (L403-408) and canvas
renders after (L410-748), each canvas onto p_viewport->render_target via
RSG::canvas->render_canvas (L714). _draw_3d (L305-336) calls render_camera (L332), which enters
RendererSceneRenderRD::render_scene (renderer_rd/renderer_scene_render_rd.cpp L1359) and ends in
_render_scene(&render_data, clear_color) (L1504). Forward+'s _render_scene
(renderer_rd/forward_clustered/render_forward_clustered.cpp L1704) finishes with
_render_buffers_post_process_and_tonemap (L2550), after the debug-draw hook (L2546). That function
(renderer_scene_render_rd.cpp L455-898) reads only the 3D internal buffer, color_texture =
rb->get_upscaled_texture() or rb->get_internal_texture() (L491), and writes the viewport render
target framebuffer (L783-788), or an intermediate "Tonemapper/destination" texture plus a copy when
a 3D scaling pass or SMAA is active (L771-776, L864-895). The shader bindings are the internal
color buffer, an auto-exposure buffer, a glow buffer, and a color-correction LUT
(shaders/effects/tonemap.glsl L47-56); no canvas texture can enter. The dispatch is
RendererRD::ToneMapper::tonemapper (renderer_rd/effects/tone_mapper.cpp L117,
tone_mapper.h L212). When can_draw_3d is false and the background is not BG_CANVAS, no 3D render
runs at all (renderer_viewport.cpp L403), so the tonemap pass never executes; a 2D-only viewport
applies no tonemap, exposure, glow, or adjustments anywhere. Canvas rendering is not part of
render_scene, and nothing color-related runs between the canvas draws and the screen blit
(draw_viewports builds the blit list at renderer_viewport.cpp L931-951 and executes it at
L980-984).

2. **The pass carries the tonemap curve, exposure, glow, FXAA, adjustments, color correction, the
sRGB encode, and debanding; the adjustments have no consumer outside it.** The settings assembly
(renderer_scene_render_rd.cpp L679-767) reads tonemap_mode, white, exposure, and
tonemapper_params from the Environment (L738-746), glow (L691-721), auto exposure (L683-689),
FXAA (L723), brightness/contrast/saturation when adjustments_enabled (L755-758), the color
correction texture (L759-763), debanding (L794-808), and convert_to_srgb = !using_hdr (L752). The
shader (tonemap.glsl) applies exposure (L864-870), FXAA (L873-876), glow (L878-907), the curve
(L893; Linear, Reinhard, Filmic, ACES in L92-144, AgX in L179-228 on the allenwp curve L149-173;
the mode enum is at servers/rendering/rendering_server_enums.h L672-676 and 4.7 adds AgX), the BCS
block (L911-941), the color-correction LUT (L933-941), linear_to_srgb when the target is 8-bit
(L942-943), and 8-bit debanding (L949-952). The pass runs even with tonemap mode LINEAR, the
Environment default (doc/classes/Environment.xml, tonemap_mode default 0): that run is what
encodes the linear 3D buffer into the 8-bit sRGB-convention target. A tree-wide grep finds
environment_get_adjustments_* consumed only at renderer_scene_render_rd.cpp L755-763 (this pass)
and L964-971 (the Mobile subpass variant, _post_process_subpass, L900-986). No other consumer
exists on this tag.

3. **On the stock path the canvas fragment output reaches the target unchanged except for alpha
blending; the engine applies no color conversion to the drawn texture.** The canvas fragment
samples the texture raw and outputs it raw (shaders/canvas.glsl: color *= texture(...) at L638,
frag_color = color at L859; no encode or decode step exists, 2D lighting only mixes). The canvas
renderer sends modulate as-is when the target is not HDR (renderer_canvas_render_rd.cpp L692-695,
conversion gated on use_linear_colors) and converts the clear color only when the target is HDR
(L2278-2284). Batch textures bind through canvas_texture_get_info
(renderer_rd/storage_rd/texture_storage.cpp L860-905), which uses the plain view unless the sRGB
view is requested (L901); the request flag is TextureState.linear_colors
(renderer_canvas_render_rd.cpp L3319-3325), which is render_target_is_using_hdr (L672, L760). The
default-material setup binds with that flag false (L2056). Custom canvas materials get two uniform
sets, a linear set and an srgb set (L1705-1706), and a non-HDR target binds the srgb set (L2327),
whose textures are plain views (material textures get sRGB views only in the linear set,
renderer_rd/storage_rd/material_storage.cpp L977-985). The pipeline applies blending only
(renderer_canvas_render_rd.cpp L1503-1520). This is the draw-path half of
godot-rd-external-texture-semantics.md finding 3.

4. **rendering/viewport/hdr_2d changes the 2D target format and switches canvas to linear colors
with a present-time encode; with it on, 2D content is engine-side color processed.** The setting
is defined at rendering_server.cpp L3740 (GLOBAL_DEF_BASIC, default false), read for the root
viewport at startup (scene_tree.cpp L2118-2119), and settable per viewport as Viewport.use_hdr_2d
(viewport.cpp L1313-1316, ClassDB bind L5167-5168, RenderingServer bind L2897), landing in
render_target_set_use_hdr (renderer_viewport.cpp L1412-1421, texture_storage.cpp L4671-4687). It
changes: the 2D target format, R16G16B16A16_SFLOAT instead of R8G8B8A8_UNORM
(texture_storage.cpp L5285-5291); canvas into linear colors, modulate converted (L694), clear
color converted (L2282), canvas material color uniforms converted sRGB-to-linear unless the
uniform opts out with HINT_COLOR_CONVERSION_DISABLED (material_storage.cpp L798-801, L448-462;
defaults convert only for HINT_SOURCE_COLOR, L806), and material plus canvas textures bound
through _SRGB views where the format has one (material_storage.cpp L977-985, texture_storage.cpp
L901);
the 3D internal buffer forced to RGBA16F (render_scene_buffers_rd.cpp L159,
render_scene_buffers_rd.h L195); the tonemap itself, convert_to_srgb false and max_value taken
from the window instead of 1.0 (renderer_scene_render_rd.cpp L736, L752); and the present path,
where blit_render_targets_to_screen sets source_is_srgb = !render_target_is_using_hdr
(renderer_rd/renderer_compositor_rd.cpp L110) and shaders/blit.glsl applies linear_to_srgb plus
optional debanding dither when the source is not sRGB and the swapchain is SDR (blit.glsl
L176-188). The engine docs state the same contract for the setting (doc/classes/ProjectSettings.xml
L3511-3514: "2D rendering will be performed on linear values and will be converted using the
appropriate transfer function immediately before blitting to the screen"). Stated loudly, with
hdr_2d on canvas-drawn content is converted sRGB-to-linear at draw setup and encoded
linear-to-sRGB at present, with an optional dither. That is engine-side color processing of 2D
content in a real configuration. ADR 0006 keeps hdr_2d off, and with it off none of this runs.
The blit takes no
conversion branch (blit.glsl L151, L173-189) and the canvas shader output lands in the target
byte for byte. HDR window output forces hdr_2d on the main viewport (window.cpp L1980-1988,
scene_tree.cpp L2115-2122, doc/classes/DisplayServer.xml L2377), so hdr_2d-off with an HDR
swapchain cannot occur on the root viewport.

5. **BG_CANVAS feeds canvas pixels through the tonemap pass; it is the one mode where stock tonemap
and adjustments process canvas content.** _draw_viewport reads the background mode
(renderer_viewport.cpp L363-372); for ENV_BG_CANVAS it renders the canvas layers below
canvas_max_layer into the render target first, then runs the 3D render mid-loop (render_canvas at
L714 for those layers, then _draw_3d or render_empty_scene at L729, L743, with the layer check at
L673-683 and L722-733). Forward+ then copies that content into the 3D color framebuffer:
render_forward_clustered.cpp L2052-2059 calls copy_to_fb_rect with convert_to_linear =
!render_target_is_using_hdr (L2055), and that flag drives FLAG_LINEAR, which applies srgb_to_linear
in the copy shader (shaders/effects/copy_to_fb.glsl L185-186; copy_to_fb_rect's p_linear parameter
in copy_effects.h). Everything downstream is the normal 3D pipeline, including the tonemap pass
and the adjustments (findings 1-2). Canvas layers at or above canvas_max_layer draw on top after
the 3D render, since the canvas loop continues past the mid-loop _draw_3d call. BG_CANVAS
tonemaps even with no camera, because render_empty_scene still runs the full render_scene
(renderer_scene_cull.cpp L3757-3774, called from renderer_viewport.cpp L678, L727, L743). The
engine docs agree that 2D sits inside the 3D pipeline in this mode (doc/classes/Viewport.xml
L466: "2D rendering is not affected by debanding unless the Environment.background_mode is
BG_CANVAS"). Stated loudly, in a viewport whose Environment uses BG_CANVAS, canvas content on a
layer below canvas_max_layer passes through the tonemap pass and the adjustments. For atlas-rt
this happens only if the host game's viewport Environment picks BG_CANVAS and the extension's
canvas layer sits below that cutoff; on the normal ordering the extension's rect draws after the
tonemap (finding 1) and carries no engine-side color op (finding 3). Inference: in that
BG_CANVAS case the extension's already-encoded output would be re-processed by the stock tonemap,
a second color pipeline over the first; the reach is source fact, the double-application
consequence is arithmetic.

6. **ImageTexture and Texture2DRD are color-identical on this path; the texture format decides,
the wrapper does not.** Both reach the canvas shader through the same two binding paths, the batch
set (canvas_texture_get_info, texture_storage.cpp L860-905, and _prepare_batch_texture_info,
renderer_canvas_render_rd.cpp L3314-3354) or a material uniform set (renderer_canvas_render_rd.cpp
L2327, textures resolved at material_storage.cpp L867-985), and both bind the plain view unless
the linear-colors mode requests the _SRGB shared view (texture_storage.cpp L901, L985, L2246-2256).
Texture2DRD keeps the caller's format (texture_rd_initialize, texture_storage.cpp L2163-2244),
and _texture_format_from_rd maps R16G16B16A16_SFLOAT to Image::FORMAT_RGBAH with no _SRGB
companion (L3003-3011; rd_format_srgb defaults to DATA_FORMAT_MAX, texture_storage.h L232-244), so
no sRGB view exists and sampling returns the stored halves raw in both hdr_2d modes. ImageTexture
maps FORMAT_RGBAH to R16G16B16A16_SFLOAT with no srgb companion (L2410-2417), same raw sampling.
FORMAT_RGBA8 maps to R8G8B8A8_UNORM with R8G8B8A8_SRGB as companion (L2320-2327) and gets an _SRGB
shared view at creation (L1043-1050); that view binds only in linear-colors mode, hdr_2d on. So
for the two delivery formats ADR 0006 uses, both RGBA16F, the zero-copy Texture2DRD and the
FORMAT_RGBAH ImageTexture are color-identical through the whole canvas path in every mode. The
wrapper changes nothing; the only engine-side conversion tied to format is the _SRGB view for byte
formats under hdr_2d, which applies to both wrappers and never to the 16F pair. This extends
godot-rd-external-texture-semantics.md finding 3 to the fallback texture.

7. **Between the canvas draw and the swapchain stand only the blit, an MSAA resolve, and straight
copies; with hdr_2d off and a non-BG_CANVAS background, no stock pass transforms canvas colors.**
The present path for a screen-attached viewport is the blit list (renderer_viewport.cpp L931-951,
executed L980-984; on RD the canvas always lands in the render target first, direct-to-screen is a
low-end-only path, L997 and L1182-1203), then RendererCompositorRD::blit_render_targets_to_screen
(renderer_compositor_rd.cpp L42-121) and shaders/blit.glsl. With hdr_2d off and an SDR window the
blit is a textured copy with no conversion branch (blit.glsl L151, L173-189). With HDR output it
applies srgb_to_linear, clamps to output_max_value, and scales by the reference multiplier
(L155-172). XR uses the same blit list (renderer_viewport.cpp L876-921). 2D MSAA resolves only
(renderer_canvas_render_rd.cpp L2276, renderer_viewport.cpp L755-758), no color math. Canvas
backbuffer copies for BackBufferCopy, CanvasGroup, and screen-texture shaders are straight copies
with an 8-bit destination flag, no conversion (texture_storage.cpp L5091-5140; copy_to_rect's
seventh parameter is p_8_bit_dst, copy_effects.cpp L404). Compositor effects hook only 3D stages:
PRE_OPAQUE, POST_OPAQUE, POST_SKY, PRE_TRANSPARENT, POST_TRANSPARENT
(render_forward_clustered.cpp L2173, L2270, L2332, L2408, L2458; Mobile L1154, L1329, L1381); no
canvas callback type exists on this tag, so a user CompositorEffect cannot stand on the canvas
path. Debanding from rendering/anti_aliasing/quality/use_debanding feeds the root viewport
(scene_tree.cpp L2131-2132) and the render target flag (renderer_viewport.cpp L1469-1477); it
dithers at the blit only in the hdr path (blit.glsl L186-188) and at the tonemap only when the
target is not HDR (renderer_scene_render_rd.cpp L794-808), so with hdr_2d off it never touches
canvas content. scaling_3d, screen-space AA, TAA, SSAO/SSIL, glow, and DoF all live inside
render_scene (findings 1-2), 3D-only. Net: the canvas-to-swapchain path carries no stock color
processing for the configuration atlas-rt runs, and the two settings that add one, hdr_2d and
BG_CANVAS, are both outside that configuration by the ADR's choices.

## Sources note

All load-bearing claims cite files of tag 4.7.2-stable, HEAD
ed1daf0bf001b61586d9930840f2f1394092c079, read from a local extraction of the complete tag source
(downloaded as the tag tarball after the sandbox TLS path blocked direct fetches; the same
schannel failure the earlier fact-find recorded). Line numbers are per file and per tag as cited.
The tag identity is verified through both the GitHub refs API and the tarball's version.py.
Documented-behavior quotes come from doc/classes/*.xml in the same tag. Items marked inference:
the BG_CANVAS double-pipeline consequence for the extension's output. Claims scoped by method:
Mobile renderer statements are dispatch-level; absence claims (adjustments consumers, compositor
callback types, blit conversion branches) rest on greps over the full extracted source tree.
Related: docs/research/godot-rd-external-texture-semantics.md (texture wrap semantics, swapchain
formats), docs/research/godot-renderer-integration.md (delivery seam).
