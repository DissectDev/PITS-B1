package clusters

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"sync"

	"pits.io/server/internal/api"
	"pits.io/server/internal/kube"
)

type rawK8sObject struct {
	APIVersion string `json:"apiVersion"`
	Kind       string `json:"kind"`
	Metadata   struct {
		Name        string            `json:"name"`
		Namespace   string            `json:"namespace"`
		Labels      map[string]string `json:"labels"`
		Annotations map[string]string `json:"annotations"`
	} `json:"metadata"`
	Spec   map[string]any `json:"spec"`
	Status map[string]any `json:"status"`
}

func fetchCrossplaneInventory(ctx context.Context, kc *kube.Client) *api.CrossplaneInventory {
	var (
		xrds         []api.CrossplaneXRDInfo
		xrdSpecs     []xrdProbeTarget
		compositions []api.CrossplaneResourceInfo
		providers    []api.CrossplanePackageInfo
		functions    []api.CrossplanePackageInfo
		wg           sync.WaitGroup
	)
	wg.Add(4)
	go func() {
		defer wg.Done()
		xrds, xrdSpecs = fetchCrossplaneXRDs(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		compositions = fetchCrossplaneCompositions(ctx, kc)
	}()
	go func() {
		defer wg.Done()
		providers = fetchCrossplanePackages(ctx, kc, "/apis/pkg.crossplane.io/v1/providers", "Provider")
	}()
	go func() {
		defer wg.Done()
		functions = fetchCrossplanePackages(ctx, kc, "/apis/pkg.crossplane.io/v1/functions", "Function")
	}()
	wg.Wait()

	if len(xrds) == 0 && len(compositions) == 0 && len(providers) == 0 && len(functions) == 0 {
		return nil
	}

	// Prioritize PITS & core custom XRDs first so large clusters never truncate key domain resources.
	sort.SliceStable(xrdSpecs, func(i, j int) bool {
		pi := xrdGroupPriority(xrdSpecs[i].group)
		pj := xrdGroupPriority(xrdSpecs[j].group)
		if pi != pj {
			return pi < pj
		}
		return xrdSpecs[i].group < xrdSpecs[j].group
	})
	if len(xrdSpecs) > 16 {
		xrdSpecs = xrdSpecs[:16]
	}

	type endpointProbe struct {
		path       string
		role       string
		defaultGVK string
		kind       string
	}
	var probes []endpointProbe
	for _, x := range xrdSpecs {
		if x.claimPlural != "" {
			probes = append(probes, endpointProbe{
				path:       fmt.Sprintf("/apis/%s/%s/%s", x.group, x.version, x.claimPlural),
				role:       "claim",
				defaultGVK: fmt.Sprintf("%s/%s", x.group, x.version),
				kind:       x.claimKind,
			})
		}
		if x.xrPlural != "" {
			probes = append(probes, endpointProbe{
				path:       fmt.Sprintf("/apis/%s/%s/%s", x.group, x.version, x.xrPlural),
				role:       "composite",
				defaultGVK: fmt.Sprintf("%s/%s", x.group, x.version),
				kind:       x.xrKind,
			})
		}
	}

	var (
		resMu     sync.Mutex
		resources []api.CrossplaneResourceInfo
		probeWg   sync.WaitGroup
	)
	for _, p := range probes {
		probeWg.Add(1)
		go func(ep endpointProbe) {
			defer probeWg.Done()
			items := fetchDynamicCrossplaneObjects(ctx, kc, ep.path, ep.role, ep.defaultGVK, ep.kind)
			if len(items) == 0 {
				return
			}
			resMu.Lock()
			resources = append(resources, items...)
			resMu.Unlock()
		}(p)
	}
	probeWg.Wait()

	resources = append(resources, compositions...)
	sort.SliceStable(resources, func(i, j int) bool {
		if resources[i].Role != resources[j].Role {
			return crossplaneRoleRank(resources[i].Role) < crossplaneRoleRank(resources[j].Role)
		}
		if resources[i].APIVersion != resources[j].APIVersion {
			return resources[i].APIVersion < resources[j].APIVersion
		}
		if resources[i].Kind != resources[j].Kind {
			return resources[i].Kind < resources[j].Kind
		}
		if resources[i].Namespace != resources[j].Namespace {
			return resources[i].Namespace < resources[j].Namespace
		}
		return resources[i].Name < resources[j].Name
	})

	pkgs := append(providers, functions...)
	sort.SliceStable(pkgs, func(i, j int) bool {
		if pkgs[i].Kind != pkgs[j].Kind {
			return pkgs[i].Kind < pkgs[j].Kind
		}
		return pkgs[i].Name < pkgs[j].Name
	})

	return &api.CrossplaneInventory{
		Installed: true,
		XRDs:      xrds,
		Packages:  pkgs,
		Resources: resources,
	}
}

func xrdGroupPriority(group string) int {
	switch group {
	case "storage.ai.pits.io", "iam.corp.io", "lab.pits.io", "platform.edge.router", "edge.router":
		return 0
	default:
		if strings.Contains(group, "pits.io") {
			return 1
		}
		return 2
	}
}

func crossplaneRoleRank(role string) int {
	switch role {
	case "claim":
		return 0
	case "composite":
		return 1
	case "composition":
		return 2
	default:
		return 3
	}
}

type xrdProbeTarget struct {
	group       string
	version     string
	xrKind      string
	xrPlural    string
	claimKind   string
	claimPlural string
}

func fetchCrossplaneXRDs(ctx context.Context, kc *kube.Client) ([]api.CrossplaneXRDInfo, []xrdProbeTarget) {
	var raw struct {
		Items []rawK8sObject `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/apis/apiextensions.crossplane.io/v1/compositeresourcedefinitions", &raw); err != nil {
		return nil, nil
	}
	xrds := make([]api.CrossplaneXRDInfo, 0, len(raw.Items))
	targets := make([]xrdProbeTarget, 0, len(raw.Items))
	for _, it := range raw.Items {
		group := strField(it.Spec, "group")
		version := pickXRDVersion(it.Spec)
		xrKind := nestedStr(it.Spec, "names", "kind")
		xrPlural := nestedStr(it.Spec, "names", "plural")
		claimKind := nestedStr(it.Spec, "claimNames", "kind")
		claimPlural := nestedStr(it.Spec, "claimNames", "plural")
		est, _ := conditionState(it.Status, "Established")
		off, _ := conditionState(it.Status, "Offered")
		xrds = append(xrds, api.CrossplaneXRDInfo{
			Name:        it.Metadata.Name,
			Group:       group,
			Version:     version,
			Kind:        xrKind,
			Plural:      xrPlural,
			ClaimKind:   claimKind,
			ClaimPlural: claimPlural,
			Established: est,
			Offered:     off,
		})
		if group != "" && version != "" && (xrPlural != "" || claimPlural != "") {
			targets = append(targets, xrdProbeTarget{
				group:       group,
				version:     version,
				xrKind:      xrKind,
				xrPlural:    xrPlural,
				claimKind:   claimKind,
				claimPlural: claimPlural,
			})
		}
	}
	sort.SliceStable(xrds, func(i, j int) bool { return xrds[i].Name < xrds[j].Name })
	return xrds, targets
}

func fetchCrossplanePackages(ctx context.Context, kc *kube.Client, path, pkgKind string) []api.CrossplanePackageInfo {
	var raw struct {
		Items []rawK8sObject `json:"items"`
	}
	if err := kc.GetJSON(ctx, path, &raw); err != nil {
		return nil
	}
	out := make([]api.CrossplanePackageInfo, 0, len(raw.Items))
	for _, it := range raw.Items {
		inst, _ := conditionState(it.Status, "Installed")
		healthy, _ := conditionState(it.Status, "Healthy")
		out = append(out, api.CrossplanePackageInfo{
			Kind:      pkgKind,
			Name:      it.Metadata.Name,
			Package:   strField(it.Spec, "package"),
			Installed: inst,
			Healthy:   healthy,
		})
	}
	return out
}

func fetchCrossplaneCompositions(ctx context.Context, kc *kube.Client) []api.CrossplaneResourceInfo {
	var raw struct {
		Items []rawK8sObject `json:"items"`
	}
	if err := kc.GetJSON(ctx, "/apis/apiextensions.crossplane.io/v1/compositions", &raw); err != nil {
		return nil
	}
	out := make([]api.CrossplaneResourceInfo, 0, len(raw.Items))
	for _, it := range raw.Items {
		highlights := map[string]string{}
		if ctr := nestedStr(it.Spec, "compositeTypeRef", "kind"); ctr != "" {
			highlights["compositeTypeKind"] = ctr
		}
		if ctv := nestedStr(it.Spec, "compositeTypeRef", "apiVersion"); ctv != "" {
			highlights["compositeTypeApiVersion"] = ctv
		}
		if mode := strField(it.Spec, "mode"); mode != "" {
			highlights["mode"] = mode
		}
		out = append(out, api.CrossplaneResourceInfo{
			APIVersion: firstNonEmpty(it.APIVersion, "apiextensions.crossplane.io/v1"),
			Kind:       firstNonEmpty(it.Kind, "Composition"),
			Name:       it.Metadata.Name,
			Role:       "composition",
			Synced:     true,
			Ready:      true,
			Highlights: highlights,
		})
	}
	return out
}

func fetchDynamicCrossplaneObjects(ctx context.Context, kc *kube.Client, path, role, defaultGVK, defaultKind string) []api.CrossplaneResourceInfo {
	var raw struct {
		APIVersion string         `json:"apiVersion"`
		Kind       string         `json:"kind"`
		Items      []rawK8sObject `json:"items"`
	}
	if err := kc.GetJSON(ctx, path, &raw); err != nil {
		return nil
	}
	out := make([]api.CrossplaneResourceInfo, 0, len(raw.Items))
	for _, it := range raw.Items {
		apiVer := firstNonEmpty(it.APIVersion, defaultGVK)
		kind := firstNonEmpty(it.Kind, defaultKind)
		synced, _ := conditionState(it.Status, "Synced")
		ready, readyReason := conditionState(it.Status, "Ready")
		compName := nestedStr(it.Spec, "compositionRef", "name")
		if compName == "" {
			compName = nestedStr(it.Spec, "crossplane", "compositionRef", "name")
		}
		resRefKind := nestedStr(it.Spec, "resourceRef", "kind")
		resRefName := nestedStr(it.Spec, "resourceRef", "name")
		resRef := ""
		if resRefName != "" {
			if resRefKind != "" {
				resRef = resRefKind + "/" + resRefName
			} else {
				resRef = resRefName
			}
		} else if compAnnot := it.Metadata.Annotations["crossplane.io/composite"]; compAnnot != "" {
			resRef = compAnnot
		}

		highlights := extractCrossplaneHighlights(kind, it.Spec, it.Status)
		out = append(out, api.CrossplaneResourceInfo{
			APIVersion:  apiVer,
			Kind:        kind,
			Namespace:   it.Metadata.Namespace,
			Name:        it.Metadata.Name,
			Role:        role,
			Composition: compName,
			ResourceRef: resRef,
			Synced:      synced,
			Ready:       ready,
			ReadyReason: readyReason,
			Highlights:  highlights,
		})
	}
	return out
}

func extractCrossplaneHighlights(kind string, spec, status map[string]any) map[string]string {
	h := map[string]string{}
	add := func(k, v string) {
		if strings.TrimSpace(v) != "" {
			h[k] = v
		}
	}
	// AIStorageWorkspace / XAIStorageWorkspace
	add("mcpEndpoint", strField(status, "mcpEndpoint"))
	add("internalEndpoint", strField(status, "internalEndpoint"))
	add("profileConfigMap", strField(status, "profileConfigMap"))
	add("credentialSecret", strField(status, "credentialSecret"))
	add("environment", strField(spec, "environment"))
	add("domain", strField(spec, "domain"))
	if rawArch, ok := status["readyArchetypes"].([]any); ok && len(rawArch) > 0 {
		parts := make([]string, 0, len(rawArch))
		for _, a := range rawArch {
			if s, ok := a.(string); ok && s != "" {
				parts = append(parts, s)
			}
		}
		if len(parts) > 0 {
			add("readyArchetypes", strings.Join(parts, ","))
		}
	}
	// AuthFlowBundle / XAuthFlowBundle
	add("activeFlowAlias", strField(status, "activeFlowAlias"))
	add("flowAlias", strField(spec, "flowAlias"))
	add("realm", strField(spec, "realm"))
	// LabDevStack / XLabDevStack
	add("externalIp", strField(spec, "externalIp"))
	if giteaURL := strField(status, "giteaUrl"); giteaURL != "" {
		add("giteaUrl", giteaURL)
	}
	if len(h) == 0 {
		return nil
	}
	return h
}

func pickXRDVersion(spec map[string]any) string {
	rawVers, ok := spec["versions"].([]any)
	if !ok {
		return ""
	}
	fallback := ""
	for _, entry := range rawVers {
		m, ok := entry.(map[string]any)
		if !ok {
			continue
		}
		name, _ := m["name"].(string)
		served, _ := m["served"].(bool)
		ref, _ := m["referenceable"].(bool)
		if served && ref && name != "" {
			return name
		}
		if fallback == "" && name != "" {
			fallback = name
		}
	}
	return fallback
}

func conditionState(status map[string]any, condType string) (bool, string) {
	rawConds, ok := status["conditions"].([]any)
	if !ok {
		return false, ""
	}
	for _, entry := range rawConds {
		m, ok := entry.(map[string]any)
		if !ok {
			continue
		}
		t, _ := m["type"].(string)
		if t != condType {
			continue
		}
		s, _ := m["status"].(string)
		r, _ := m["reason"].(string)
		return strings.EqualFold(s, "True"), r
	}
	return false, ""
}

func strField(m map[string]any, key string) string {
	if m == nil {
		return ""
	}
	v, ok := m[key]
	if !ok || v == nil {
		return ""
	}
	switch t := v.(type) {
	case string:
		return t
	case json.Number:
		return t.String()
	default:
		return ""
	}
}

func nestedStr(m map[string]any, keys ...string) string {
	cur := m
	for i, k := range keys {
		if cur == nil {
			return ""
		}
		if i == len(keys)-1 {
			return strField(cur, k)
		}
		next, ok := cur[k].(map[string]any)
		if !ok {
			return ""
		}
		cur = next
	}
	return ""
}
