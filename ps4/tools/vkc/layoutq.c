/* layoutq: asks radv (fake GFX7 device) for descriptor set layout binding offsets through VK_EXT_descriptor_buffer, to check
 * they equal the offsets radv's compiled shaders use with ordinary sets (read from RADV_DEBUG=shaders NIR). PS4 port tooling.
 *   layoutq TYPES...   e.g. layoutq uniform texture sampler   (binding i gets TYPES[i]) */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <string.h>
#define CK(x) do { VkResult r_ = (x); if (r_) { fprintf(stderr, "%s: %d\n", #x, r_); return 1; } } while (0)
int main(int argc, char **argv) {
	VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "layoutq", 1, NULL, 0, VK_API_VERSION_1_3};
	VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app};
	VkInstance inst; CK(vkCreateInstance(&ici, NULL, &inst));
	uint32_t n = 1; VkPhysicalDevice pd; vkEnumeratePhysicalDevices(inst, &n, &pd);
	float pri = 1;
	VkDeviceQueueCreateInfo q = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, 0, 1, &pri};
	VkPhysicalDeviceDescriptorBufferFeaturesEXT fdb = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DESCRIPTOR_BUFFER_FEATURES_EXT, NULL, VK_TRUE};
	VkPhysicalDeviceVulkan12Features f12 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, &fdb};
	f12.bufferDeviceAddress = VK_TRUE;
	const char *ext[] = {"VK_EXT_descriptor_buffer"};
	VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, &f12, 0, 1, &q, 0, NULL, 1, ext, NULL};
	VkDevice d; CK(vkCreateDevice(pd, &dci, NULL, &d));
	VkDescriptorSetLayoutBinding b[16]; int nb = argc - 1;
	for (int i = 0; i < nb; i++) {
		const char *t = argv[1 + i];
		VkDescriptorType ty = !strcmp(t, "uniform") ? VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER : !strcmp(t, "storage") ? VK_DESCRIPTOR_TYPE_STORAGE_BUFFER
			: !strcmp(t, "texture") ? VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE : !strcmp(t, "storage_texture") ? VK_DESCRIPTOR_TYPE_STORAGE_IMAGE : VK_DESCRIPTOR_TYPE_SAMPLER;
		b[i] = (VkDescriptorSetLayoutBinding){i, ty, 1, VK_SHADER_STAGE_ALL, NULL};
	}
	VkDescriptorSetLayoutCreateInfo lci = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, VK_DESCRIPTOR_SET_LAYOUT_CREATE_DESCRIPTOR_BUFFER_BIT_EXT, nb, b};
	VkDescriptorSetLayout l; CK(vkCreateDescriptorSetLayout(d, &lci, NULL, &l));
	PFN_vkGetDescriptorSetLayoutBindingOffsetEXT off = (void *)vkGetDeviceProcAddr(d, "vkGetDescriptorSetLayoutBindingOffsetEXT");
	PFN_vkGetDescriptorSetLayoutSizeEXT size = (void *)vkGetDeviceProcAddr(d, "vkGetDescriptorSetLayoutSizeEXT");
	VkDeviceSize s; size(d, l, &s);
	printf("set size %llu:", (unsigned long long)s);
	for (int i = 0; i < nb; i++) { VkDeviceSize o; off(d, l, i, &o); printf(" %d(%s)=%llu", i, argv[1 + i], (unsigned long long)o); }
	printf("\n");
	return 0;
}
