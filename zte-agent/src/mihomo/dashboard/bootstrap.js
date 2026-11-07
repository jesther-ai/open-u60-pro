// Prepended only to the pinned dashboard module, never the agent's pages.
const u60Dashboard = (() => {
    const uuid = "u60-pro-managed";
    const backend = () => ({
        type: "clash",
        protocol: location.protocol === "https:" ? "https" : "http",
        host: location.hostname,
        port: location.port || (location.protocol === "https:" ? "443" : "80"),
        secondaryPath: "/ui/mihomo/api",
        password: "",
        label: "U60 Pro",
        disableUpgradeCore: true,
        disableTunMode: true,
    });
    const backends = (saved) => [
        ...(Array.isArray(saved) ? saved.filter((b) => b && typeof b === "object" && typeof b.uuid === "string" && b.uuid !== uuid) : []),
        { ...backend(), uuid },
    ];
    let storage;
    try {
        storage = window.localStorage;
        const probe = "u60/dashboard-storage-check";
        storage.getItem(probe);
        storage.setItem(probe, "1");
        storage.removeItem(probe);
    } catch {
        // Private/blocked storage still gets a working backend for this page.
        storage = Object.create(null);
        Object.defineProperties(storage, {
            getItem: { value: (key) => storage[key] ?? null },
            setItem: { value: (key, value) => { storage[key] = String(value); } },
            removeItem: { value: (key) => { delete storage[key]; } },
        });
    }
    return { uuid, backend, backends, storage };
})();
// A module-local binding: do not replace the browser's global storage.
const localStorage = u60Dashboard.storage;
