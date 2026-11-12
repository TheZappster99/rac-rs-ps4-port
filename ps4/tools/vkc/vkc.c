/* vkc: compiles rac-rs shaders for the PS4's GPU generation (GFX7) with Mesa radv on a fake device (WSL, Mesa's AMDGPU
 * drm-shim: LD_PRELOAD=libamdgpu_noop_drm_shim.so AMDGPU_GPU_ID=bonaire). PS4 port tooling, PS4_GPU_DESIGN.md.
 *
 *   vkc BASE_VS BASE_FS      a graphics pipeline from BASE_VS.spv/.txt and BASE_FS.spv/.txt (from wgsl2spv)
 *   vkc BASE_CS              a compute pipeline
 *
 * The .txt reflection gives the bindings (merged into descriptor set layouts, every stage), the vertex inputs (one vertex
 * buffer binding each) and the fragment outputs (RGBA8 colour attachments; plus a D32 depth attachment). Prints radv's
 * statistics for every compiled stage (VK_KHR_pipeline_executable_properties). RADV_DEBUG=shaders adds the GCN code. */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CK(x) do { VkResult r_ = (x); if (r_ != VK_SUCCESS) { fprintf(stderr, "%s failed: %d\n", #x, r_); exit(1); } } while (0)

typedef struct { int set, binding, count; VkDescriptorType type; } Bind;
static Bind binds[64];
static int nbinds;
typedef struct { int loc; VkFormat fmt; int size; } In;
static In ins[16];
static int nins, nouts, depth_out;

static void *slurp(const char *path, size_t *len) {
	FILE *f = fopen(path, "rb");
	if (!f) { fprintf(stderr, "cannot open %s\n", path); exit(1); }
	fseek(f, 0, SEEK_END); *len = ftell(f); fseek(f, 0, SEEK_SET);
	void *p = malloc(*len + 1);
	fread(p, 1, *len, f); fclose(f);
	((char *)p)[*len] = 0;
	return p;
}

static VkFormat fmt_of(const char *t, int *size) {
	struct { const char *n; VkFormat f; int s; } m[] = {
		{"f32", VK_FORMAT_R32_SFLOAT, 4}, {"f32x2", VK_FORMAT_R32G32_SFLOAT, 8}, {"f32x3", VK_FORMAT_R32G32B32_SFLOAT, 12}, {"f32x4", VK_FORMAT_R32G32B32A32_SFLOAT, 16},
		{"u32", VK_FORMAT_R32_UINT, 4}, {"u32x2", VK_FORMAT_R32G32_UINT, 8}, {"u32x3", VK_FORMAT_R32G32B32_UINT, 12}, {"u32x4", VK_FORMAT_R32G32B32A32_UINT, 16},
		{"i32", VK_FORMAT_R32_SINT, 4}, {"i32x2", VK_FORMAT_R32G32_SINT, 8}, {"i32x3", VK_FORMAT_R32G32B32_SINT, 12}, {"i32x4", VK_FORMAT_R32G32B32A32_SINT, 16},
	};
	for (unsigned i = 0; i < sizeof m / sizeof m[0]; i++)
		if (!strcmp(t, m[i].n)) { *size = m[i].s; return m[i].f; }
	fprintf(stderr, "unknown vertex format %s\n", t); exit(1);
}

/* Reads BASE.txt; returns the stage. */
static VkShaderStageFlagBits reflect(const char *base) {
	char path[1024]; size_t len;
	snprintf(path, sizeof path, "%s.txt", base);
	char *t = slurp(path, &len);
	VkShaderStageFlagBits st = VK_SHADER_STAGE_COMPUTE_BIT;
	for (char *line = strtok(t, "\n"); line; line = strtok(NULL, "\n")) {
		char a[32], b[32]; int x, y, n;
		if (sscanf(line, "stage %31s", a) == 1)
			st = !strcmp(a, "vertex") ? VK_SHADER_STAGE_VERTEX_BIT : !strcmp(a, "fragment") ? VK_SHADER_STAGE_FRAGMENT_BIT : VK_SHADER_STAGE_COMPUTE_BIT;
		else if (sscanf(line, "bind %d %d %31s %d", &x, &y, a, &n) == 4) {
			VkDescriptorType ty = !strcmp(a, "uniform") ? VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER
				: !strncmp(a, "storage_ro", 10) || !strcmp(a, "storage") ? VK_DESCRIPTOR_TYPE_STORAGE_BUFFER
				: !strcmp(a, "texture") ? VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE
				: !strcmp(a, "storage_texture") ? VK_DESCRIPTOR_TYPE_STORAGE_IMAGE
				: !strcmp(a, "sampler") ? VK_DESCRIPTOR_TYPE_SAMPLER : VK_DESCRIPTOR_TYPE_MAX_ENUM;
			if (ty == VK_DESCRIPTOR_TYPE_MAX_ENUM) { fprintf(stderr, "%s: binding %d.%d is %s (not on GFX7)\n", base, x, y, a); exit(3); }
			int dup = 0;
			for (int i = 0; i < nbinds; i++) dup |= binds[i].set == x && binds[i].binding == y;
			if (!dup) binds[nbinds++] = (Bind){x, y, n, ty};
		} else if (sscanf(line, "in %d %31s", &x, b) == 2) {
			int sz; VkFormat f = fmt_of(b, &sz);
			ins[nins++] = (In){x, f, sz};
		} else if (sscanf(line, "out %d", &x) == 1) {
			if (x + 1 > nouts) nouts = x + 1;
		} else if (!strcmp(line, "depth"))
			depth_out = 1;
	}
	free(t);
	return st;
}

/* The entry point keeps its WGSL name: the part of BASE after the last dot. */
static const char *entry_of(const char *base) {
	const char *dot = strrchr(base, '.');
	return dot ? dot + 1 : "main";
}

static VkShaderModule module(VkDevice d, const char *base) {
	char path[1024]; size_t len;
	snprintf(path, sizeof path, "%s.spv", base);
	void *code = slurp(path, &len);
	VkShaderModuleCreateInfo ci = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, len, code};
	VkShaderModule m;
	CK(vkCreateShaderModule(d, &ci, NULL, &m));
	free(code);
	return m;
}

int main(int argc, char **argv) {
	if (argc < 2) { fprintf(stderr, "usage: vkc BASE_VS BASE_FS | vkc BASE_CS\n"); return 2; }
	VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "vkc", 1, NULL, 0, VK_API_VERSION_1_3};
	VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app};
	VkInstance inst;
	CK(vkCreateInstance(&ici, NULL, &inst));
	uint32_t n = 1; VkPhysicalDevice pd;
	vkEnumeratePhysicalDevices(inst, &n, &pd);
	if (!n) { fprintf(stderr, "no device\n"); return 1; }
	VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd, &props);
	uint32_t ne = 0; vkEnumerateDeviceExtensionProperties(pd, NULL, &ne, NULL);
	VkExtensionProperties *ex = calloc(ne, sizeof *ex); vkEnumerateDeviceExtensionProperties(pd, NULL, &ne, ex);
	int has_bin = 0, has_exec = 0;
	for (uint32_t i = 0; i < ne; i++) {
		has_bin |= !strcmp(ex[i].extensionName, "VK_KHR_pipeline_binary");
		has_exec |= !strcmp(ex[i].extensionName, "VK_KHR_pipeline_executable_properties");
	}
	printf("device: %s; VK_KHR_pipeline_binary %s, executable properties %s\n", props.deviceName, has_bin ? "yes" : "no", has_exec ? "yes" : "no");

	float pri = 1.0f;
	VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, 0, 1, &pri};
	VkPhysicalDevicePipelineExecutablePropertiesFeaturesKHR fexec = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PIPELINE_EXECUTABLE_PROPERTIES_FEATURES_KHR, NULL, VK_TRUE};
	VkPhysicalDeviceVulkan13Features f13 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES, &fexec};
	f13.dynamicRendering = VK_TRUE;
	VkPhysicalDeviceVulkan12Features f12 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, &f13};
	VkPhysicalDeviceVulkan11Features f11 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES, &f12};
	VkPhysicalDeviceFeatures2 feat = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, &f11};
	/* Turn on everything the device has (the viewer's shaders use what wgpu enabled on the PC). */
	vkGetPhysicalDeviceFeatures2(pd, &feat);
	f13.dynamicRendering = VK_TRUE; fexec.pipelineExecutableInfo = VK_TRUE;
	/* VKC_DUMP=DIR: also export the compiled pipeline as VK_KHR_pipeline_binary blobs (radv's serialized shaders: config,
	 * shader info with the precomputed registers and user SGPR locations, statistics, GCN code) to DIR/<name>.<n>.bin. */
	const char *dump = getenv("VKC_DUMP");
	VkPhysicalDevicePipelineBinaryFeaturesKHR fbin = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PIPELINE_BINARY_FEATURES_KHR, NULL, VK_TRUE};
	VkPhysicalDeviceMaintenance5FeaturesKHR fm5 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_5_FEATURES_KHR, &fbin, VK_TRUE};
	if (dump) fexec.pNext = &fm5;
	const char *dext[] = {"VK_KHR_pipeline_executable_properties", "VK_KHR_maintenance5", "VK_KHR_pipeline_binary"};
	VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, &feat, 0, 1, &qci, 0, NULL, dump ? 3 : 1, dext, NULL};
	VkDevice d;
	CK(vkCreateDevice(pd, &dci, NULL, &d));

	VkShaderStageFlagBits st[2]; int nst = argc - 1 > 2 ? 2 : argc - 1;
	for (int i = 0; i < nst; i++) st[i] = reflect(argv[1 + i]);

	/* Descriptor set layouts (sets 0..max), every binding visible to every stage. */
	int maxset = -1;
	for (int i = 0; i < nbinds; i++) if (binds[i].set > maxset) maxset = binds[i].set;
	VkDescriptorSetLayout sl[8];
	for (int s = 0; s <= maxset; s++) {
		VkDescriptorSetLayoutBinding b[64]; int nb = 0;
		for (int i = 0; i < nbinds; i++)
			if (binds[i].set == s) b[nb++] = (VkDescriptorSetLayoutBinding){binds[i].binding, binds[i].type, binds[i].count, VK_SHADER_STAGE_ALL, NULL};
		VkDescriptorSetLayoutCreateInfo lci = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, 0, nb, b};
		CK(vkCreateDescriptorSetLayout(d, &lci, NULL, &sl[s]));
	}
	VkPushConstantRange pc = {VK_SHADER_STAGE_ALL, 0, 128};
	VkPipelineLayoutCreateInfo plci = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, maxset + 1, sl, 1, &pc};
	VkPipelineLayout pl;
	CK(vkCreatePipelineLayout(d, &plci, NULL, &pl));

	VkPipeline p;
	/* With VKC_DUMP the pipeline keeps its data for VK_KHR_pipeline_binary (flags2 replaces the flags field). */
	VkPipelineCreateFlags2CreateInfoKHR f2 = {VK_STRUCTURE_TYPE_PIPELINE_CREATE_FLAGS_2_CREATE_INFO_KHR, NULL, VK_PIPELINE_CREATE_2_CAPTURE_DATA_BIT_KHR | VK_PIPELINE_CREATE_2_CAPTURE_STATISTICS_BIT_KHR};
	if (st[0] == VK_SHADER_STAGE_COMPUTE_BIT) {
		VkComputePipelineCreateInfo cci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, dump ? (void *)&f2 : NULL, VK_PIPELINE_CREATE_CAPTURE_STATISTICS_BIT_KHR,
			{VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, VK_SHADER_STAGE_COMPUTE_BIT, module(d, argv[1]), entry_of(argv[1])}, pl};
		CK(vkCreateComputePipelines(d, VK_NULL_HANDLE, 1, &cci, NULL, &p));
	} else {
		VkPipelineShaderStageCreateInfo ss[2];
		for (int i = 0; i < nst; i++)
			ss[i] = (VkPipelineShaderStageCreateInfo){VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, st[i], module(d, argv[1 + i]), entry_of(argv[1 + i])};
		VkVertexInputBindingDescription vb[16]; VkVertexInputAttributeDescription va[16];
		for (int i = 0; i < nins; i++) {
			vb[i] = (VkVertexInputBindingDescription){i, ins[i].size, VK_VERTEX_INPUT_RATE_VERTEX};
			va[i] = (VkVertexInputAttributeDescription){ins[i].loc, i, ins[i].fmt, 0};
		}
		VkPipelineVertexInputStateCreateInfo vi = {VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, NULL, 0, nins, vb, nins, va};
		VkPipelineInputAssemblyStateCreateInfo ia = {VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, NULL, 0, VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
		VkPipelineViewportStateCreateInfo vp = {VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, NULL, 0, 1, NULL, 1, NULL};
		VkPipelineRasterizationStateCreateInfo rs = {VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO};
		rs.lineWidth = 1.0f;
		VkPipelineMultisampleStateCreateInfo ms = {VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, NULL, 0, VK_SAMPLE_COUNT_1_BIT};
		VkPipelineDepthStencilStateCreateInfo ds = {VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO, NULL, 0, VK_TRUE, VK_TRUE, VK_COMPARE_OP_GREATER_OR_EQUAL};
		VkPipelineColorBlendAttachmentState cb[8];
		VkFormat cf[8];
		for (int i = 0; i < nouts; i++) {
			cb[i] = (VkPipelineColorBlendAttachmentState){VK_TRUE, VK_BLEND_FACTOR_SRC_ALPHA, VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA, VK_BLEND_OP_ADD, VK_BLEND_FACTOR_ONE, VK_BLEND_FACTOR_ZERO, VK_BLEND_OP_ADD, 0xf};
			/* VKC_COLOR=bgra8: the PS4 display buffers' format (the export format radv bakes into the PS follows it). */
			const char *cfmt = getenv("VKC_COLOR");
			cf[i] = cfmt && !strcmp(cfmt, "bgra8") ? VK_FORMAT_B8G8R8A8_UNORM : VK_FORMAT_R16G16B16A16_SFLOAT;
			if (cfmt && !strcmp(cfmt, "bgra8")) cb[i].blendEnable = VK_FALSE;
		}
		VkPipelineColorBlendStateCreateInfo cbs = {VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, NULL, 0, VK_FALSE, 0, nouts, cb};
		VkDynamicState dyn[] = {VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR};
		VkPipelineDynamicStateCreateInfo dy = {VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, NULL, 0, 2, dyn};
		VkPipelineRenderingCreateInfo ri = {VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO, NULL, 0, nouts, cf, VK_FORMAT_D32_SFLOAT};
		f2.pNext = &ri;
		VkGraphicsPipelineCreateInfo gci = {VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, dump ? (void *)&f2 : (void *)&ri, VK_PIPELINE_CREATE_CAPTURE_STATISTICS_BIT_KHR, nst, ss,
			&vi, &ia, NULL, &vp, &rs, &ms, &ds, &cbs, &dy, pl};
		CK(vkCreateGraphicsPipelines(d, VK_NULL_HANDLE, 1, &gci, NULL, &p));
	}

	/* What radv compiled. */
	PFN_vkGetPipelineExecutablePropertiesKHR gep = (void *)vkGetDeviceProcAddr(d, "vkGetPipelineExecutablePropertiesKHR");
	PFN_vkGetPipelineExecutableStatisticsKHR ges = (void *)vkGetDeviceProcAddr(d, "vkGetPipelineExecutableStatisticsKHR");
	VkPipelineInfoKHR pinfo = {VK_STRUCTURE_TYPE_PIPELINE_INFO_KHR, NULL, p};
	uint32_t nx = 0; gep(d, &pinfo, &nx, NULL);
	VkPipelineExecutablePropertiesKHR px[8];
	for (uint32_t i = 0; i < nx; i++) px[i] = (VkPipelineExecutablePropertiesKHR){VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_PROPERTIES_KHR};
	gep(d, &pinfo, &nx, px);
	for (uint32_t i = 0; i < nx; i++) {
		printf("  %s:", px[i].name);
		VkPipelineExecutableInfoKHR ei = {VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_INFO_KHR, NULL, p, i};
		uint32_t ns = 0; ges(d, &ei, &ns, NULL);
		VkPipelineExecutableStatisticKHR sx[64];
		for (uint32_t k = 0; k < ns && k < 64; k++) sx[k] = (VkPipelineExecutableStatisticKHR){VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_STATISTIC_KHR};
		if (ns > 64) ns = 64;
		ges(d, &ei, &ns, sx);
		for (uint32_t k = 0; k < ns; k++) {
			const char *nm = sx[k].name;
			if (strstr(nm, "GPRs") || strstr(nm, "Code size") || strstr(nm, "Instructions") || strstr(nm, "Spill") || strstr(nm, "Scratch"))
				printf(" %s %llu;", nm, (unsigned long long)sx[k].value.u64);
		}
		printf("\n");
	}
	if (dump) {
		PFN_vkCreatePipelineBinariesKHR cpb = (void *)vkGetDeviceProcAddr(d, "vkCreatePipelineBinariesKHR");
		PFN_vkGetPipelineBinaryDataKHR gpb = (void *)vkGetDeviceProcAddr(d, "vkGetPipelineBinaryDataKHR");
		VkPipelineBinaryCreateInfoKHR bci = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_CREATE_INFO_KHR, NULL, NULL, p, NULL};
		VkPipelineBinaryHandlesInfoKHR hi = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_HANDLES_INFO_KHR, NULL, 0, NULL};
		CK(cpb(d, &bci, NULL, &hi));
		VkPipelineBinaryKHR bins[8];
		hi.pPipelineBinaries = bins;
		CK(cpb(d, &bci, NULL, &hi));
		const char *name = strrchr(argv[1], '/');
		name = name ? name + 1 : argv[1];
		for (uint32_t i = 0; i < hi.pipelineBinaryCount; i++) {
			VkPipelineBinaryDataInfoKHR di = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_DATA_INFO_KHR, NULL, bins[i]};
			VkPipelineBinaryKeyKHR key = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_KEY_KHR};
			size_t sz = 0;
			CK(gpb(d, &di, &key, &sz, NULL));
			void *buf = malloc(sz);
			CK(gpb(d, &di, &key, &sz, buf));
			char path[1024];
			snprintf(path, sizeof path, "%s/%s.%u.bin", dump, name, i);
			FILE *f = fopen(path, "wb");
			if (f) { fwrite(buf, 1, sz, f); fclose(f); }
			printf("  binary %u: %zu bytes -> %s\n", i, sz, path);
			free(buf);
		}
	}
	printf("OK\n");
	return 0;
}
