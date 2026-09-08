/**************************************************************************/
/*  register_types.cpp                                                    */
/**************************************************************************/
/*                        This file is part of:                           */
/*                          ATLAS-RT RENDERER                             */
/**************************************************************************/

#include "register_types.h"

#include "external_memory_hooks.h"

void initialize_vulkanhooks_module(ModuleInitializationLevel p_level) {
	if (p_level == MODULE_INITIALIZATION_LEVEL_SERVERS) {
		// First construction installs the VulkanHooks singleton. The object
		// is never freed: the base ctor is the only installer and the dtor
		// would clear the singleton while the renderer still consults it.
		memnew(ExternalMemoryHooks());
	}
}

void uninitialize_vulkanhooks_module(ModuleInitializationLevel p_level) {
}
