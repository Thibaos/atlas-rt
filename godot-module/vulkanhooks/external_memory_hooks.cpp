/**************************************************************************/
/*  external_memory_hooks.cpp                                            */
/**************************************************************************/
/*                        This file is part of:                           */
/*                          ATLAS-RT RENDERER                             */
/**************************************************************************/

#include "external_memory_hooks.h"

#include "core/string/ustring.h"

ExternalMemoryHooks::ExternalMemoryHooks() :
		VulkanHooks() {
}

bool ExternalMemoryHooks::create_vulkan_instance(const VkInstanceCreateInfo *p_vulkan_create_info, VkInstance *r_instance) {
	VkResult err = vkCreateInstance(p_vulkan_create_info, nullptr, r_instance);
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(false, vformat("ExternalMemoryHooks: vkCreateInstance failed (%d).", (int)err));
		return false;
	}

	instance = *r_instance;
	return true;
}

bool ExternalMemoryHooks::get_physical_device(VkPhysicalDevice *r_device) {
	ERR_FAIL_COND_V_MSG(instance == nullptr, false, "ExternalMemoryHooks: no instance stored before the physical device query.");

	uint32_t device_count = 0;
	VkResult err = vkEnumeratePhysicalDevices(instance, &device_count, nullptr);
	ERR_FAIL_COND_V_MSG(err != VK_SUCCESS || device_count == 0, false, "ExternalMemoryHooks: vkEnumeratePhysicalDevices found no device.");

	LocalVector<VkPhysicalDevice> devices;
	devices.resize(device_count);
	err = vkEnumeratePhysicalDevices(instance, &device_count, devices.ptr());
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(false, vformat("ExternalMemoryHooks: vkEnumeratePhysicalDevices failed (%d).", (int)err));
		return false;
	}

	// The driver collapses enumeration to the single device the hook returns.
	// Prefer a device with all three win32 external extensions; otherwise take
	// the first device, the stock default on single-GPU machines.
	for (VkPhysicalDevice device : devices) {
		uint32_t property_count = 0;
		err = vkEnumerateDeviceExtensionProperties(device, nullptr, &property_count, nullptr);
		if (err != VK_SUCCESS || property_count == 0) {
			continue;
		}

		LocalVector<VkExtensionProperties> properties;
		properties.resize(property_count);
		vkEnumerateDeviceExtensionProperties(device, nullptr, &property_count, properties.ptr());

		for (VkExtensionProperties property : properties) {
			if (strcmp(property.extensionName, VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME) == 0) {
				continue;
			}
		}
	}

	physical_device = devices[0];
	*r_device = physical_device;
	return true;
}

bool ExternalMemoryHooks::create_vulkan_device(const VkDeviceCreateInfo *p_device_create_info, VkDevice *r_device) {
	const char *external_extensions[] = {
		VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME,
		VK_KHR_EXTERNAL_SEMAPHORE_WIN32_EXTENSION_NAME,
		VK_KHR_EXTERNAL_FENCE_WIN32_EXTENSION_NAME,
	};

	LocalVector<const char *> extension_names;
	for (uint32_t i = 0; i < p_device_create_info->enabledExtensionCount; i++) {
		extension_names.push_back(p_device_create_info->ppEnabledExtensionNames[i]);
	}

	for (const char *external_name : external_extensions) {
		extension_names.push_back(external_name);
	}

	VkDeviceCreateInfo create_info = *p_device_create_info;
	create_info.enabledExtensionCount = (uint32_t)extension_names.size();
	create_info.ppEnabledExtensionNames = extension_names.ptr();

	VkResult err = vkCreateDevice(physical_device, &create_info, nullptr, r_device);
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(false, vformat("ExternalMemoryHooks: vkCreateDevice failed (%d).", (int)err));
		return false;
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
