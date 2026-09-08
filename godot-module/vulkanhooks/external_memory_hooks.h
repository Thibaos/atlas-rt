#pragma once

#include "core/templates/local_vector.h"

#include <drivers/vulkan/vulkan_hooks.h>

class ExternalMemoryHooks : public VulkanHooks {
public:
	virtual bool create_vulkan_instance(const VkInstanceCreateInfo *p_vulkan_create_info, VkInstance *r_instance) override;
	virtual bool get_physical_device(VkPhysicalDevice *r_device) override;
	virtual bool create_vulkan_device(const VkDeviceCreateInfo *p_device_create_info, VkDevice *r_device) override;
	virtual void set_direct_queue_family_and_index(uint32_t p_queue_family_index, uint32_t p_queue_index) override;
	virtual bool use_fragment_density_offsets() override;
	virtual void get_fragment_density_offsets(LocalVector<VkOffset2D> &r_offsets, const Vector2i &p_granularity) override;
	virtual bool use_subsampled_images() override;

private:
	static constexpr const char *EXTERNAL_EXTENSIONS[] = {
		VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME,
		VK_KHR_EXTERNAL_SEMAPHORE_WIN32_EXTENSION_NAME,
		VK_KHR_EXTERNAL_FENCE_WIN32_EXTENSION_NAME,
	};

	VkInstance instance = nullptr;
	VkPhysicalDevice physical_device = nullptr;
};

