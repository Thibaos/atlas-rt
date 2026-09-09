#pragma once

#include "core/object/class_db.h"
#include "core/templates/local_vector.h"

#include "external_memory_hooks.h"

class VulkanHooksBridge : public Object {
	GDCLASS(VulkanHooksBridge, Object)

	struct Entries {
		int slot = 0;
		uint64_t width = 0;
		uint64_t height = 0;
		VkImage image = VK_NULL_HANDLE;
		VkDeviceMemory memory = VK_NULL_HANDLE;
		RID rid;
		uint64_t grave_from = UINT64_MAX;
	};

	LocalVector<Entries> entries;

	RID create_entry(int p_slot, uint64_t p_memory_handle, uint64_t p_memory_size, uint64_t p_memory_type_index, uint64_t p_width, uint64_t p_height);
	void destroy_entry(Entries &p_entry);
	void purge_graves(uint64_t p_now);

protected:
	static void _bind_methods();

public:
	RID create_image(int64_t p_slot, int64_t p_memory_handle, int64_t p_memory_size, int64_t p_memory_type_index, int64_t p_width, int64_t p_height);
	void release_all();

	VulkanHooksBridge();
	~VulkanHooksBridge();
};
