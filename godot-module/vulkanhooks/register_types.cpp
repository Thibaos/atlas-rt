/**************************************************************************/
/*  ATLAS-RT RENDERER                                                     */
/**************************************************************************/

#include "register_types.h"

#include "bridge.h"

#include "core/config/engine.h"
#include "core/object/class_db.h"

#include "external_memory_hooks.h"

static VulkanHooksBridge *atlas_bridge = nullptr;

void initialize_vulkanhooks_module(ModuleInitializationLevel p_level) {
	if (p_level == MODULE_INITIALIZATION_LEVEL_SERVERS) {
		// First construction installs the VulkanHooks singleton. The object
		// is never freed: the base ctor is the only installer and the dtor
		// would clear the singleton while the renderer still consults it.
		memnew(ExternalMemoryHooks());

		GDREGISTER_CLASS(VulkanHooksBridge);
		atlas_bridge = memnew(VulkanHooksBridge);
		Engine::get_singleton()->add_singleton(Engine::Singleton("VulkanHooksBridge", atlas_bridge));
	}
}

void uninitialize_vulkanhooks_module(ModuleInitializationLevel p_level) {
	if (p_level == MODULE_INITIALIZATION_LEVEL_SERVERS && atlas_bridge) {
		memdelete(atlas_bridge);
		atlas_bridge = nullptr;
	}
}
