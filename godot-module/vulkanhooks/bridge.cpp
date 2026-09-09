/**************************************************************************/
/*  ATLAS-RT RENDERER                                                     */
/**************************************************************************/

#include "bridge.h"

#include "core/config/engine.h"
#include "core/string/ustring.h"

#include "servers/rendering/rendering_device.h"
#include "servers/rendering/rendering_server.h"

#include <cstdint>

VulkanHooksBridge::VulkanHooksBridge() {
}

VulkanHooksBridge::~VulkanHooksBridge() {
	uint64_t now = Engine::get_singleton()->get_frames_drawn();
	(void)now;

	for (Entries &entry : entries) {
		destroy_entry(entry);
	}
}

void VulkanHooksBridge::_bind_methods() {
	ClassDB::bind_method(D_METHOD("create_image", "slot", "memory_handle", "memory_size", "memory_type_index", "width", "height"), &VulkanHooksBridge::create_image);
	ClassDB::bind_method(D_METHOD("release_all"), &VulkanHooksBridge::release_all);
}

void VulkanHooksBridge::destroy_entry(Entries &p_entry) {
	VulkanHooks *hooks = VulkanHooks::get_singleton();
	if (!hooks) {
		return;
	}

	VkDevice device = static_cast<ExternalMemoryHooks *>(hooks)->get_vulkan_device();
	if (device) {
		if (p_entry.image != VK_NULL_HANDLE) {
			vkDestroyImage(device, p_entry.image, nullptr);
		}

		if (p_entry.memory != VK_NULL_HANDLE) {
			vkFreeMemory(device, p_entry.memory, nullptr);
		}
	}

	p_entry.image = VK_NULL_HANDLE;
	p_entry.memory = VK_NULL_HANDLE;
}

void VulkanHooksBridge::release_all() {
	uint64_t now = Engine::get_singleton()->get_frames_drawn();

	for (Entries &entry : entries) {
		if (entry.grave_from == UINT64_MAX) {
			entry.grave_from = now;
		}
	}
}

RID VulkanHooksBridge::create_entry(int p_slot, uint64_t p_memory_handle, uint64_t p_memory_size, uint64_t p_memory_type_index, uint64_t p_width, uint64_t p_height) {
	VulkanHooks *hooks = VulkanHooks::get_singleton();
	if (!hooks) {
		ERR_FAIL_V_MSG(RID(), "VulkanHooksBridge: hooks singleton not installed.");
	}

	VkDevice device = static_cast<ExternalMemoryHooks *>(hooks)->get_vulkan_device();
	if (!device) {
		ERR_FAIL_V_MSG(RID(), "VulkanHooksBridge: no Vulkan device captured during engine device creation.");
	}

	uint64_t now = Engine::get_singleton()->get_frames_drawn();
	RID reused_rid;
	bool retire_existing = false;

	for (Entries &entry : entries) {
		if (entry.grave_from != UINT64_MAX && now > entry.grave_from + 8) {
			if (entry.image != VK_NULL_HANDLE) {
				destroy_entry(entry);
				entry.image = VK_NULL_HANDLE;
				entry.memory = VK_NULL_HANDLE;
			}
		}

		if (entry.grave_from == UINT64_MAX && entry.slot == p_slot) {
			if (entry.width == p_width && entry.height == p_height) {
				reused_rid = entry.rid;
				break;
			}

			retire_existing = true;
		}
	}

	if (reused_rid.is_valid()) {
		return reused_rid;
	}

	if (retire_existing) {
		for (Entries &entry : entries) {
			if (entry.grave_from == UINT64_MAX && entry.slot == p_slot) {
				entry.grave_from = now;
			}
		}
	}

	VkImageCreateInfo image_info = {};
	image_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
	image_info.imageType = VK_IMAGE_TYPE_2D;
	image_info.format = VK_FORMAT_R16G16B16A16_SFLOAT;
	image_info.extent = { (uint32_t)p_width, (uint32_t)p_height, 1 };
	image_info.mipLevels = 1;
	image_info.arrayLayers = 1;
	image_info.samples = VK_SAMPLE_COUNT_1_BIT;
	image_info.tiling = VK_IMAGE_TILING_OPTIMAL;
	image_info.usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
	image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	image_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

	VkExternalMemoryImageCreateInfo external_info = {};
	external_info.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
	external_info.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT;
	image_info.pNext = &external_info;

	VkImage image = VK_NULL_HANDLE;
	VkResult err = vkCreateImage(device, &image_info, nullptr, &image);
	if (err != VK_SUCCESS) {
		ERR_FAIL_V_MSG(RID(), String("VulkanHooksBridge: vkCreateImage failed (") + itos(err) + ").");
	}

	VkMemoryRequirements requirements = {};
	vkGetImageMemoryRequirements(device, image, &requirements);

	if (requirements.size > p_memory_size) {
		vkDestroyImage(device, image, nullptr);
		ERR_FAIL_V_MSG(RID(), String("VulkanHooksBridge: imported allocation too small for the delivery image (") + itos(requirements.size) + " > " + itos(p_memory_size) + ").");
	}

	uint32_t memory_type = (uint32_t)p_memory_type_index;

	if (!(requirements.memoryTypeBits & (1u << memory_type))) {
		for (uint32_t bit = 0; bit < 32; bit++) {
			if (requirements.memoryTypeBits & (1u << bit)) {
				WARN_PRINT("VulkanHooksBridge: atlas memory type not allowed on the engine device; using bit " + itos(bit) + ".");
				memory_type = bit;
				break;
			}
		}
	}

	VkImportMemoryWin32HandleInfoKHR import_info = {};
	import_info.sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR;
	import_info.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT;
	import_info.handle = (HANDLE)(uintptr_t)p_memory_handle;

	VkMemoryAllocateInfo allocate_info = {};
	allocate_info.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	allocate_info.pNext = &import_info;
	allocate_info.allocationSize = p_memory_size;
	allocate_info.memoryTypeIndex = memory_type;

	VkDeviceMemory memory = VK_NULL_HANDLE;
	err = vkAllocateMemory(device, &allocate_info, nullptr, &memory);
	if (err != VK_SUCCESS) {
		vkDestroyImage(device, image, nullptr);
		ERR_FAIL_V_MSG(RID(), String("VulkanHooksBridge: vkAllocateMemory with imported handle failed (") + itos(err) + ").");
	}

	err = vkBindImageMemory(device, image, memory, 0);
	if (err != VK_SUCCESS) {
		vkFreeMemory(device, memory, nullptr);
		vkDestroyImage(device, image, nullptr);
		ERR_FAIL_V_MSG(RID(), String("VulkanHooksBridge: vkBindImageMemory failed (") + itos(err) + ").");
	}

	RenderingServer *server = RenderingServer::get_singleton();
	RenderingDevice *rd = server->get_rendering_device();

	if (!rd) {
		vkFreeMemory(device, memory, nullptr);
		vkDestroyImage(device, image, nullptr);
		ERR_FAIL_V_MSG(RID(), "VulkanHooksBridge: no RenderingDevice from the rendering server.");
	}

	RID rid = rd->texture_create_from_extension(
			RenderingDevice::TEXTURE_TYPE_2D,
			RenderingDevice::DATA_FORMAT_R16G16B16A16_SFLOAT,
			RenderingDevice::TEXTURE_SAMPLES_1,
			RD::TEXTURE_USAGE_SAMPLING_BIT | RD::TEXTURE_USAGE_STORAGE_BIT | RD::TEXTURE_USAGE_CAN_COPY_FROM_BIT,
			(uint64_t)image,
			p_width,
			p_height,
			1,
			1,
			1);

	if (!rid.is_valid()) {
		vkFreeMemory(device, memory, nullptr);
		vkDestroyImage(device, image, nullptr);
		ERR_FAIL_V_MSG(RID(), "VulkanHooksBridge: texture_create_from_extension returned an invalid RID.");
	}

	entries.push_back({ p_slot, p_width, p_height, image, memory, rid, UINT64_MAX });
	return rid;
}

RID VulkanHooksBridge::create_image(int64_t p_slot, int64_t p_memory_handle, int64_t p_memory_size, int64_t p_memory_type_index, int64_t p_width, int64_t p_height) {
	if (p_slot < 0 || p_memory_handle <= 0 || p_memory_size <= 0 || p_memory_type_index < 0 || p_width <= 0 || p_height <= 0) {
		ERR_FAIL_V_MSG(RID(), "VulkanHooksBridge: create_image arguments out of range.");
	}

	return create_entry((int)p_slot, (uint64_t)p_memory_handle, (uint64_t)p_memory_size, (uint64_t)p_memory_type_index, (uint64_t)p_width, (uint64_t)p_height);
}
