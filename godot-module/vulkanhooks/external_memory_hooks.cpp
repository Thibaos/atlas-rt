/**************************************************************************/
/*  external_memory_hooks.cpp                                            */
/**************************************************************************/
/*                        This file is part of:                           */
/*                          ATLAS-RT RENDERER                             */
/**************************************************************************/

#include "external_memory_hooks.h"

#include "core/string/ustring.h"

#include <cstring>

#include <iterator>

bool ExternalMemoryHooks::create_vulkan_instance(const VkInstanceCreateInfo *p_vulkan_create_info, VkInstance *r_instance) {
	VkResult err = vkCreateInstance(p_vulkan_create_info, nullptr, r_instance);
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(false, vformat("ExternalMemoryHooks: vkCreateInstance failed (%d).", (int)err));
	}

	instance = *r_instance;
	return true;
}

bool ExternalMemoryHooks::get_physical_device(VkPhysicalDevice *r_device) {
	if (instance == nullptr) {
		ERR_FAIL_V_MSG(false, "ExternalMemoryHooks: no instance stored before the physical device query.");
	}

	uint32_t device_count = 0;
	VkResult err = vkEnumeratePhysicalDevices(instance, &device_count, nullptr);
	if (err != VK_SUCCESS || device_count == 0) {
		ERR_FAIL_V_MSG(false, "ExternalMemoryHooks: vkEnumeratePhysicalDevices found no device.");
	}

	LocalVector<VkPhysicalDevice> devices;
	devices.resize(device_count);
	err = vkEnumeratePhysicalDevices(instance, &device_count, devices.ptr());
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(false, vformat("ExternalMemoryHooks: vkEnumeratePhysicalDevices failed (%d).", (int)err));
	}

	// The driver collapses enumeration to the single device the hook returns.
	// Prefer a device exposing all three win32 external extensions; otherwise
	// fall back to the first device, the stock default on single-GPU machines.
	VkPhysicalDevice fallback_device = devices[0];
	VkPhysicalDevice candidate = nullptr;

	for (VkPhysicalDevice device : devices) {
		uint32_t count = 0;
		VkResult prop_err = vkEnumerateDeviceExtensionProperties(device, nullptr, &count, nullptr);
		if (prop_err != VK_SUCCESS || count == 0) {
			continue;
		}

		LocalVector<VkExtensionProperties> properties;
		properties.resize(count);
		vkEnumerateDeviceExtensionProperties(device, nullptr, &count, properties.ptr());

		uint32_t supported = 0;
		for (VkExtensionProperties property : properties) {
			for (const char *external_name : EXTERNAL_EXTENSIONS) {
				if (strcmp(property.extensionName, external_name) == 0) {
					supported++;
					break;
				}
			}
		}

		if (supported == (sizeof(EXTERNAL_EXTENSIONS) / sizeof(EXTERNAL_EXTENSIONS[0]))) {
			candidate = device;
			break;
		}
	}

	if (candidate == nullptr) {
		WARN_PRINT("ExternalMemoryHooks: no device supports the win32 external-memory extensions; using the first device as fallback.");
	}

	physical_device = candidate != nullptr ? candidate : fallback_device;
	*r_device = physical_device;
	return true;
}

bool ExternalMemoryHooks::create_vulkan_device(const VkDeviceCreateInfo *p_device_create_info, VkDevice *r_device) {
	LocalVector<const char *> extension_names;
	for (uint32_t i = 0; i < p_device_create_info->enabledExtensionCount; i++) {
		extension_names.push_back(p_device_create_info->ppEnabledExtensionNames[i]);
	}

	for (const char *external_name : EXTERNAL_EXTENSIONS) {
		extension_names.push_back(external_name);
	}

	// The stock pQueuePriorities arrays are driver-owned and left untouched:
	// the hook adds no queue, and the array lives through vkCreateDevice.
	VkDeviceCreateInfo create_info = *p_device_create_info;
	create_info.enabledExtensionCount = (uint32_t)extension_names.size();
	create_info.ppEnabledExtensionNames = extension_names.ptr();

	VkResult err = vkCreateDevice(physical_device, &create_info, nullptr, r_device);
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(false, vformat("ExternalMemoryHooks: vkCreateDevice failed (%d).", (int)err));
	}

	return true;
}

void ExternalMemoryHooks::set_direct_queue_family_and_index(uint32_t p_queue_family_index, uint32_t p_queue_index) {
}

bool ExternalMemoryHooks::use_fragment_density_offsets() {
	return false;
}

void ExternalMemoryHooks::get_fragment_density_offsets(LocalVector<VkOffset2D> &r_offsets, const Vector2i &p_granularity) {
	r_offsets.clear();
}

bool ExternalMemoryHooks::use_subsampled_images() {
	return false;
}
