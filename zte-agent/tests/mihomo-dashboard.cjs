// Run with: node --test zte-agent/tests/mihomo-dashboard.cjs
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const bootstrap = fs.readFileSync(path.join(__dirname, "../src/mihomo/dashboard/bootstrap.js"), "utf8");
function memoryStorage(values = {}) {
    const saved = new Map(Object.entries(values));
    return {
        getItem: (key) => saved.get(key) ?? null,
        setItem: (key, value) => saved.set(key, String(value)),
        removeItem: (key) => saved.delete(key),
    };
}
function load(url, storage = memoryStorage()) {
    const window = {};
    Object.defineProperty(window, "localStorage", { get: () => {
        if (storage instanceof Error) throw storage;
        return storage;
    } });
    const context = vm.createContext({ location: new URL(url), window });
    vm.runInContext(`${bootstrap}\nglobalThis.dashboard = u60Dashboard;`, context);
    return { ...context, dashboard: context.dashboard };
}
const plain = (v) => JSON.parse(JSON.stringify(v));

test("current browser origin supplies HTTP/HTTPS, default/custom ports and bracketed IPv6", () => {
    for (const url of [
        "http://192.168.0.1:9090", "http://192.168.0.1", "https://u60.home.arpa:9443",
        "https://u60.home.arpa", "http://[fd00::1]:9090", "https://[fd00::1]",
    ]) {
        const current = new URL(url);
        const { dashboard } = load(url + "/ui/mihomo/dashboard/#/proxies");
        const backend = dashboard.backend();
        assert.equal(`${backend.protocol}://${backend.host}:${backend.port}${backend.secondaryPath}`, `${current.origin.replace(/\/$/, "")}${current.port ? "" : current.protocol === "https:" ? ":443" : ":80"}/ui/mihomo/api`);
        assert.equal(backend.password, "");
        assert.equal(backend.type, "clash");
        assert.equal(backend.disableUpgradeCore, true);
        assert.equal(backend.disableTunMode, true);
    }
});

test("stale or hostile Setup query never overrides the router's same-origin backend", () => {
    const { dashboard } = load("https://u60.home.arpa:9443/ui/mihomo/dashboard/?hostname=evil.invalid&secret=old-secret#/setup?port=19090");
    assert.equal(dashboard.backend().host, "u60.home.arpa");
    assert.equal(dashboard.backend().port, "9443");
    assert.equal(dashboard.backend().password, "");
    assert.equal(dashboard.backend().secondaryPath, "/ui/mihomo/api");
});

test("managed backend is refreshed without losing other backends or preferences", () => {
    const storage = memoryStorage({ "config/theme": "dark", "unrelated": "retained" });
    const { dashboard, window } = load("http://192.168.0.1:9090", storage);
    const other = { uuid: "other", type: "clash", host: "192.168.8.1", label: "Other router" };
    const stale = { uuid: dashboard.uuid, host: "old-address", password: "outdated" };
    const list = plain(dashboard.backends([other, stale, stale, null, 7]));
    assert.deepEqual(list[0], other);
    assert.equal(list.length, 2);
    assert.equal(list[1].host, "192.168.0.1");
    assert.equal(list[1].uuid, dashboard.uuid);
    assert.equal(list[1].password, "");
    assert.deepEqual(plain(dashboard.backends(list)), list);
    assert.equal(storage.getItem("config/theme"), "dark");
    assert.equal(storage.getItem("unrelated"), "retained");
    assert.equal(window.localStorage, storage);
});

test("corrupt backend storage is normalized and unavailable storage is page-local", () => {
    const blocked = () => { throw new Error("Storage blocked"); };
    for (const storage of [new Error("SecurityError"), { getItem: blocked }, { getItem: () => null, setItem: blocked }]) {
        const { dashboard } = load("http://192.168.0.1:9090", storage);
        for (const malformed of [null, true, 7, {}, "invalid", [null, false]]) {
            const list = dashboard.backends(malformed);
            assert.equal(list.length, 1);
            assert.equal(list[0].uuid, dashboard.uuid);
        }
        dashboard.storage.setItem("config/test", true);
        assert.equal(dashboard.storage.getItem("config/test"), "true");
        assert.deepEqual(Object.keys(dashboard.storage), ["config/test"]);
        dashboard.storage.removeItem("config/test");
        assert.equal(dashboard.storage.getItem("config/test"), null);
        assert.equal(dashboard.backend().host, "192.168.0.1");
    }
});
