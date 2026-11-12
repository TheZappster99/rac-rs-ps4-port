/* vkrec: compiles captured rac-rs pipelines for the PS4's GPU generation (GFX7) with Mesa radv on a fake device (WSL,
 * LD_PRELOAD=libamdgpu_noop_drm_shim.so AMDGPU_GPU_ID=bonaire). PS4 port tooling, PS4_GPU_DESIGN.md.
 *
 *   vkrec CAPTURE_DIR OUT_DIR
 *
 * CAPTURE_DIR holds the records the PC game writes with RAC_PS4_CAPTURE (rac-rs/vendor/wgpu-hal/src/vulkan/ps4cap.rs has the
 * format): <key>.rec plus the SPIR-V of each stage. Each record is rebuilt as the same Vulkan pipeline (same set layouts,
 * immediates, vertex input, fixed state and attachment formats, dynamic rendering) and its compiled stages are exported with
 * VK_KHR_pipeline_binary to OUT_DIR/<key>.<n>.bin (radvbin turns them into .rsh). OUT_DIR/<key>.sets gets radv's descriptor
 * set memory layout, asked through VK_EXT_descriptor_buffer on a twin of each set layout:
 *   set S SIZE, then per binding: b S BINDING OFFSET (bytes into the set), or dyn S BINDING INDEX for a dynamic buffer
 *   (radv keeps those outside set memory, in binding order across the pipeline layout).
 * One line per record on stdout (OK / FAIL and radv's statistics); the exit code is the number of failures (max 255). */
#include <vulkan/vulkan.h>
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { unsigned binding, type, count, stages, wgpu; } RBind;
typedef struct { int nb; RBind b[64]; } RSet;
typedef struct { unsigned stage; char entry[256], file[256]; } RStage;

static VkDevice dev;
static PFN_vkGetDescriptorSetLayoutBindingOffsetEXT get_off;
static PFN_vkGetDescriptorSetLayoutSizeEXT get_size;
static PFN_vkCreatePipelineBinariesKHR create_bins;
static PFN_vkGetPipelineBinaryDataKHR get_bin;
static PFN_vkDestroyPipelineBinaryKHR destroy_bin;
static PFN_vkGetPipelineExecutablePropertiesKHR get_exec;
static PFN_vkGetPipelineExecutableStatisticsKHR get_stats;

static void *slurp(const char *path, size_t *len) {
	FILE *f = fopen(path, "rb");
	if (!f) return NULL;
	fseek(f, 0, SEEK_END); *len = ftell(f); fseek(f, 0, SEEK_SET);
	char *p = malloc(*len + 1);
	if (fread(p, 1, *len, f) != *len) { fclose(f); free(p); return NULL; }
	fclose(f);
	p[*len] = 0;
	return p;
}

static int is_dynamic(unsigned t) { return t == VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER_DYNAMIC || t == VK_DESCRIPTOR_TYPE_STORAGE_BUFFER_DYNAMIC; }

static void stats(VkPipeline p, char *out, size_t cap) {
	VkPipelineInfoKHR pi = {VK_STRUCTURE_TYPE_PIPELINE_INFO_KHR, NULL, p};
	uint32_t nx = 0; get_exec(dev, &pi, &nx, NULL);
	VkPipelineExecutablePropertiesKHR px[8];
	if (nx > 8) nx = 8;
	for (uint32_t i = 0; i < nx; i++) px[i] = (VkPipelineExecutablePropertiesKHR){VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_PROPERTIES_KHR};
	get_exec(dev, &pi, &nx, px);
	size_t n = 0;
	for (uint32_t i = 0; i < nx; i++) {
		VkPipelineExecutableInfoKHR ei = {VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_INFO_KHR, NULL, p, i};
		uint32_t ns = 0; get_stats(dev, &ei, &ns, NULL);
		VkPipelineExecutableStatisticKHR sx[64];
		if (ns > 64) ns = 64;
		for (uint32_t k = 0; k < ns; k++) sx[k] = (VkPipelineExecutableStatisticKHR){VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_STATISTIC_KHR};
		get_stats(dev, &ei, &ns, sx);
		unsigned long long sg = 0, vg = 0, code = 0, spill = 0, scratch = 0;
		for (uint32_t k = 0; k < ns; k++) {
			const char *nm = sx[k].name; unsigned long long v = sx[k].value.u64;
			if (!strcmp(nm, "SGPRs")) sg = v; else if (!strcmp(nm, "VGPRs")) vg = v; else if (!strcmp(nm, "Code size")) code = v;
			else if (strstr(nm, "Spilled")) spill += v; else if (strstr(nm, "Scratch")) scratch = v;
		}
		n += snprintf(out + n, n < cap ? cap - n : 0, " %s[s%llu v%llu %lluB%s%s]", px[i].name, sg, vg, code, spill ? " SPILL" : "", scratch ? " SCRATCH" : "");
	}
}

/* Compiles one record. Returns 0 on success. */
static int one(const char *cap, const char *out, const char *name) {
	char path[2048]; size_t len;
	snprintf(path, sizeof path, "%s/%s", cap, name);
	char *text = slurp(path, &len);
	if (!text) { printf("%s FAIL cannot read\n", name); return 1; }

	char key[32] = "", kind[16] = "", label[256] = "-";
	unsigned immediate = 0;
	RSet sets[8]; int nsets = 0, cur = -1;
	RStage st[4]; int nst = 0;
	VkVertexInputBindingDescription vb[16]; int nvb = 0;
	VkVertexInputAttributeDescription va[32]; int nva = 0;
	unsigned topo = 3, restart = 0, poly = 0, cull = 0, front = 0, dclamp = 0, bias = 0, bconst = 0, bclamp = 0, bslope = 0;
	unsigned dsf = 0, dtest = 0, dwrite = 0, dcmp = 0, sten = 0, samples = 1, a2c = 0, mlo = ~0u, mhi = ~0u;
	VkStencilOpState sface[2]; memset(sface, 0, sizeof sface);
	VkPipelineColorBlendAttachmentState cb[8]; VkFormat cf[8]; int ncb = 0;

	for (char *line = strtok(text, "\n"); line; line = strtok(NULL, "\n")) {
		unsigned a, b, c, d, e, f, g, h, i, j; int s;
		if (!strncmp(line, "key ", 4)) snprintf(key, sizeof key, "%s", line + 4);
		else if (!strncmp(line, "kind ", 5)) snprintf(kind, sizeof kind, "%s", line + 5);
		else if (!strncmp(line, "label ", 6)) snprintf(label, sizeof label, "%s", line + 6);
		else if (sscanf(line, "layout %*s %u", &a) == 1) immediate = a;
		else if (sscanf(line, "set %d %*s %u", &s, &a) == 2) {
			if (s != nsets || s >= 8) { printf("%s FAIL set %d out of order\n", name, s); free(text); return 1; }
			cur = nsets++; sets[cur].nb = 0;
		} else if (sscanf(line, "b %u %u %u %u %u", &a, &b, &c, &d, &e) == 5 && cur >= 0 && sets[cur].nb < 64)
			sets[cur].b[sets[cur].nb++] = (RBind){a, b, c, d, e};
		else if (nst < 4 && sscanf(line, "stage %u %255s %255s", &st[nst].stage, st[nst].entry, st[nst].file) == 3) nst++;
		else if (nvb < 16 && sscanf(line, "vb %u %u %u", &a, &b, &c) == 3) vb[nvb++] = (VkVertexInputBindingDescription){a, b, c};
		else if (nva < 32 && sscanf(line, "va %u %u %u %u", &a, &b, &c, &d) == 4) va[nva++] = (VkVertexInputAttributeDescription){a, b, c, d};
		else if (sscanf(line, "ia %u %u", &a, &b) == 2) { topo = a; restart = b; }
		else if (sscanf(line, "rs %u %u %u %u %u %u %u %u", &poly, &cull, &front, &dclamp, &bias, &bconst, &bclamp, &bslope) == 8) {}
		else if (sscanf(line, "ds %u %u %u %u %u", &dsf, &dtest, &dwrite, &dcmp, &sten) == 5) {}
		else if (sscanf(line, "sf %u %u %u %u %u %u %u", &a, &b, &c, &d, &e, &f, &g) == 7 && a < 2)
			sface[a] = (VkStencilOpState){b, c, d, e, f, g, 0};
		else if (sscanf(line, "ms %u %u %u %u", &samples, &a2c, &mlo, &mhi) == 4) {}
		else if (ncb < 8 && sscanf(line, "cb %u %u %u %u %u %u %u %u %u %u", &a, &b, &c, &d, &e, &f, &g, &h, &i, &j) == 10) {
			cf[ncb] = b;
			cb[ncb] = (VkPipelineColorBlendAttachmentState){c, d, e, f, g, h, i, j};
			ncb++;
		}
	}
	free(text);
	int render = !strcmp(kind, "render");
	if (!key[0] || (!render && strcmp(kind, "compute")) || !nst) { printf("%s FAIL incomplete record\n", name); return 1; }

	/* Set layouts as the PC made them, and descriptor-buffer twins for the offsets. */
	VkDescriptorSetLayout sl[8];
	FILE *sf = NULL;
	snprintf(path, sizeof path, "%s/%s.sets", out, key);
	sf = fopen(path, "w");
	unsigned dyn_index = 0;
	for (int k = 0; k < nsets; k++) {
		VkDescriptorSetLayoutBinding lb[64], tb[64]; int nt = 0;
		for (int q = 0; q < sets[k].nb; q++) {
			RBind *r = &sets[k].b[q];
			lb[q] = (VkDescriptorSetLayoutBinding){r->binding, r->type, r->count, r->stages, NULL};
			if (!is_dynamic(r->type)) tb[nt++] = lb[q];
		}
		VkDescriptorSetLayoutCreateInfo lci = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, 0, sets[k].nb, lb};
		VkResult r = vkCreateDescriptorSetLayout(dev, &lci, NULL, &sl[k]);
		if (r) { printf("%s FAIL set layout %d: %d\n", name, k, r); if (sf) fclose(sf); return 1; }
		VkDescriptorSetLayoutCreateInfo tci = {VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO, NULL, VK_DESCRIPTOR_SET_LAYOUT_CREATE_DESCRIPTOR_BUFFER_BIT_EXT, nt, tb};
		VkDescriptorSetLayout twin;
		r = vkCreateDescriptorSetLayout(dev, &tci, NULL, &twin);
		if (r) { printf("%s FAIL descriptor-buffer twin of set %d: %d\n", name, k, r); if (sf) fclose(sf); return 1; }
		VkDeviceSize size = 0; get_size(dev, twin, &size);
		if (sf) fprintf(sf, "set %d %llu\n", k, (unsigned long long)size);
		for (int q = 0; q < sets[k].nb; q++) {
			RBind *rb = &sets[k].b[q];
			if (is_dynamic(rb->type)) {
				if (sf) fprintf(sf, "dyn %d %u %u\n", k, rb->binding, dyn_index);
				dyn_index += rb->count;
			} else {
				VkDeviceSize o = 0; get_off(dev, twin, rb->binding, &o);
				if (sf) fprintf(sf, "b %d %u %llu\n", k, rb->binding, (unsigned long long)o);
			}
		}
		vkDestroyDescriptorSetLayout(dev, twin, NULL);
	}
	if (sf) fclose(sf);
	VkPushConstantRange pc = {VK_SHADER_STAGE_ALL, 0, immediate};
	VkPipelineLayoutCreateInfo plci = {VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO, NULL, 0, nsets, sl, immediate ? 1 : 0, &pc};
	VkPipelineLayout pl;
	if (vkCreatePipelineLayout(dev, &plci, NULL, &pl)) { printf("%s FAIL pipeline layout\n", name); return 1; }

	VkPipelineShaderStageCreateInfo ss[4]; VkShaderModule mods[4];
	for (int k = 0; k < nst; k++) {
		snprintf(path, sizeof path, "%s/%s", cap, st[k].file);
		void *code = slurp(path, &len);
		if (!code) { printf("%s FAIL cannot read %s\n", name, st[k].file); return 1; }
		VkShaderModuleCreateInfo mci = {VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, NULL, 0, len, code};
		VkResult r = vkCreateShaderModule(dev, &mci, NULL, &mods[k]);
		free(code);
		if (r) { printf("%s FAIL shader module %s: %d\n", name, st[k].file, r); return 1; }
		ss[k] = (VkPipelineShaderStageCreateInfo){VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, NULL, 0, st[k].stage, mods[k], st[k].entry};
	}

	VkPipelineCreateFlags2CreateInfoKHR f2 = {VK_STRUCTURE_TYPE_PIPELINE_CREATE_FLAGS_2_CREATE_INFO_KHR, NULL,
		VK_PIPELINE_CREATE_2_CAPTURE_DATA_BIT_KHR | VK_PIPELINE_CREATE_2_CAPTURE_STATISTICS_BIT_KHR};
	VkPipeline p;
	VkResult r;
	if (!render) {
		VkComputePipelineCreateInfo cci = {VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, &f2, 0, ss[0], pl};
		r = vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cci, NULL, &p);
	} else {
		VkPipelineVertexInputStateCreateInfo vi = {VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO, NULL, 0, nvb, vb, nva, va};
		VkPipelineInputAssemblyStateCreateInfo ia = {VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO, NULL, 0, topo, restart};
		VkPipelineViewportStateCreateInfo vp = {VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO, NULL, 0, 1, NULL, 1, NULL};
		float fb[3]; memcpy(&fb[0], &bconst, 4); memcpy(&fb[1], &bclamp, 4); memcpy(&fb[2], &bslope, 4);
		VkPipelineRasterizationStateCreateInfo rs = {VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO, NULL, 0, dclamp, VK_FALSE,
			poly, cull, front, bias, fb[0], fb[1], fb[2], 1.0f};
		uint32_t mask[2] = {mlo, mhi};
		VkPipelineMultisampleStateCreateInfo ms = {VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO, NULL, 0, samples, VK_FALSE, 0, mask, a2c, VK_FALSE};
		VkPipelineDepthStencilStateCreateInfo ds = {VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO, NULL, 0, dtest, dwrite, dcmp, VK_FALSE, sten, sface[0], sface[1], 0, 1};
		VkPipelineColorBlendStateCreateInfo cbs = {VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO, NULL, 0, VK_FALSE, 0, ncb, cb};
		VkDynamicState dyn[] = {VK_DYNAMIC_STATE_VIEWPORT, VK_DYNAMIC_STATE_SCISSOR, VK_DYNAMIC_STATE_BLEND_CONSTANTS, VK_DYNAMIC_STATE_STENCIL_REFERENCE};
		VkPipelineDynamicStateCreateInfo dy = {VK_STRUCTURE_TYPE_PIPELINE_DYNAMIC_STATE_CREATE_INFO, NULL, 0, 4, dyn};
		/* Depth / stencil aspects of the depth format. */
		VkFormat depth = VK_FORMAT_UNDEFINED, stencil = VK_FORMAT_UNDEFINED;
		switch (dsf) {
		case VK_FORMAT_D16_UNORM: case VK_FORMAT_X8_D24_UNORM_PACK32: case VK_FORMAT_D32_SFLOAT: depth = dsf; break;
		case VK_FORMAT_S8_UINT: stencil = dsf; break;
		case VK_FORMAT_D16_UNORM_S8_UINT: case VK_FORMAT_D24_UNORM_S8_UINT: case VK_FORMAT_D32_SFLOAT_S8_UINT: depth = stencil = dsf; break;
		default: break;
		}
		VkPipelineRenderingCreateInfo ri = {VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO, NULL, 0, ncb, cf, depth, stencil};
		f2.pNext = &ri;
		VkGraphicsPipelineCreateInfo gci = {VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO, &f2, 0, nst, ss,
			&vi, &ia, NULL, &vp, &rs, &ms, &ds, &cbs, &dy, pl};
		r = vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gci, NULL, &p);
	}
	for (int k = 0; k < nst; k++) vkDestroyShaderModule(dev, mods[k], NULL);
	if (r) { printf("%s FAIL pipeline: %d (%s)\n", name, r, label); return 1; }

	char sb[1024] = "";
	stats(p, sb, sizeof sb);

	VkPipelineBinaryCreateInfoKHR bci = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_CREATE_INFO_KHR, NULL, NULL, p, NULL};
	VkPipelineBinaryHandlesInfoKHR hi = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_HANDLES_INFO_KHR, NULL, 0, NULL};
	VkPipelineBinaryKHR bins[8];
	if (create_bins(dev, &bci, NULL, &hi) || hi.pipelineBinaryCount > 8) { printf("%s FAIL binaries\n", name); return 1; }
	hi.pPipelineBinaries = bins;
	if (create_bins(dev, &bci, NULL, &hi)) { printf("%s FAIL binaries\n", name); return 1; }
	for (uint32_t k = 0; k < hi.pipelineBinaryCount; k++) {
		VkPipelineBinaryDataInfoKHR di = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_DATA_INFO_KHR, NULL, bins[k]};
		VkPipelineBinaryKeyKHR bk = {VK_STRUCTURE_TYPE_PIPELINE_BINARY_KEY_KHR};
		size_t sz = 0;
		get_bin(dev, &di, &bk, &sz, NULL);
		void *buf = malloc(sz);
		get_bin(dev, &di, &bk, &sz, buf);
		snprintf(path, sizeof path, "%s/%s.%u.bin", out, key, k);
		FILE *f = fopen(path, "wb");
		if (f) { fwrite(buf, 1, sz, f); fclose(f); }
		free(buf);
		destroy_bin(dev, bins[k], NULL);
	}
	vkDestroyPipeline(dev, p, NULL);
	vkDestroyPipelineLayout(dev, pl, NULL);
	for (int k = 0; k < nsets; k++) vkDestroyDescriptorSetLayout(dev, sl[k], NULL);
	printf("%s OK %s %u binaries (%s)%s\n", key, kind, hi.pipelineBinaryCount, label, sb);
	return 0;
}

int main(int argc, char **argv) {
	if (argc != 3) { fprintf(stderr, "usage: vkrec CAPTURE_DIR OUT_DIR\n"); return 255; }
	VkApplicationInfo app = {VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "vkrec", 1, NULL, 0, VK_API_VERSION_1_3};
	VkInstanceCreateInfo ici = {VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app};
	VkInstance inst;
	if (vkCreateInstance(&ici, NULL, &inst)) { fprintf(stderr, "no instance\n"); return 255; }
	uint32_t n = 1; VkPhysicalDevice pd;
	vkEnumeratePhysicalDevices(inst, &n, &pd);
	if (!n) { fprintf(stderr, "no device\n"); return 255; }
	VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pd, &props);
	fprintf(stderr, "device: %s\n", props.deviceName);

	/* Every feature the device has (the PC enabled what its GPU has), plus what the export and the offsets need. */
	float pri = 1.0f;
	VkDeviceQueueCreateInfo qci = {VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, NULL, 0, 0, 1, &pri};
	VkPhysicalDeviceDescriptorBufferFeaturesEXT fdb = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DESCRIPTOR_BUFFER_FEATURES_EXT, NULL};
	VkPhysicalDevicePipelineBinaryFeaturesKHR fbin = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PIPELINE_BINARY_FEATURES_KHR, &fdb};
	VkPhysicalDeviceMaintenance5FeaturesKHR fm5 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_5_FEATURES_KHR, &fbin};
	VkPhysicalDevicePipelineExecutablePropertiesFeaturesKHR fexec = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PIPELINE_EXECUTABLE_PROPERTIES_FEATURES_KHR, &fm5};
	VkPhysicalDeviceVulkan13Features f13 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES, &fexec};
	VkPhysicalDeviceVulkan12Features f12 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, &f13};
	VkPhysicalDeviceVulkan11Features f11 = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_1_FEATURES, &f12};
	VkPhysicalDeviceFeatures2 feat = {VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, &f11};
	vkGetPhysicalDeviceFeatures2(pd, &feat);
	f13.dynamicRendering = fexec.pipelineExecutableInfo = fm5.maintenance5 = fbin.pipelineBinaries = VK_TRUE;
	fdb.descriptorBuffer = VK_TRUE; f12.bufferDeviceAddress = VK_TRUE;
	/* Not needed and they change how radv compiles: leave them off. */
	fdb.descriptorBufferCaptureReplay = fdb.descriptorBufferImageLayoutIgnored = fdb.descriptorBufferPushDescriptors = VK_FALSE;
	f12.bufferDeviceAddressCaptureReplay = VK_FALSE;
	const char *ext[] = {"VK_KHR_pipeline_executable_properties", "VK_KHR_maintenance5", "VK_KHR_pipeline_binary", "VK_EXT_descriptor_buffer"};
	VkDeviceCreateInfo dci = {VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, &feat, 0, 1, &qci, 0, NULL, 4, ext, NULL};
	VkResult r = vkCreateDevice(pd, &dci, NULL, &dev);
	if (r) { fprintf(stderr, "vkCreateDevice: %d\n", r); return 255; }
#define GET(v, n) v = (void *)vkGetDeviceProcAddr(dev, n); if (!v) { fprintf(stderr, "no %s\n", n); return 255; }
	GET(get_off, "vkGetDescriptorSetLayoutBindingOffsetEXT");
	GET(get_size, "vkGetDescriptorSetLayoutSizeEXT");
	GET(create_bins, "vkCreatePipelineBinariesKHR");
	GET(get_bin, "vkGetPipelineBinaryDataKHR");
	GET(destroy_bin, "vkDestroyPipelineBinaryKHR");
	GET(get_exec, "vkGetPipelineExecutablePropertiesKHR");
	GET(get_stats, "vkGetPipelineExecutableStatisticsKHR");

	DIR *d = opendir(argv[1]);
	if (!d) { fprintf(stderr, "cannot open %s\n", argv[1]); return 255; }
	int fails = 0, oks = 0;
	struct dirent *e;
	while ((e = readdir(d))) {
		size_t l = strlen(e->d_name);
		if (l < 5 || strcmp(e->d_name + l - 4, ".rec")) continue;
		if (one(argv[1], argv[2], e->d_name)) fails++; else oks++;
		fflush(stdout);
	}
	closedir(d);
	fprintf(stderr, "vkrec: %d compiled, %d failed\n", oks, fails);
	return fails > 255 ? 255 : fails;
}
