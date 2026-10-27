(function () {
    const tools = [
      {
        name: "get_uptime_medics_info",
        description: "Get the site name, description and API location for Uptime Medics.",
        inputSchema: { type: "object", properties: {} },
        execute: async function () {
          const res = await fetch("/.well-known/agent-card.json");
          return await res.json();
        }
      }
    ];
    function registerTools() {
      const mc = (typeof navigator !== "undefined" && navigator.modelContext) ||
        (typeof document !== "undefined" && document.modelContext);
      if (!mc) return;
      if (typeof mc.provideContext === "function") {
        try { mc.provideContext({ tools: tools }); } catch (e) {}
      }
      if (typeof mc.registerTool === "function") {
        tools.forEach(function (tool) {
          try { mc.registerTool(tool); } catch (e) {}
        });
      }
    }
    registerTools();
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", registerTools);
    }
    window.addEventListener("load", registerTools);
    let count = 0;
    const interval = setInterval(function () {
      registerTools();
      if (++count > 20) clearInterval(interval);
    }, 250);
  })();
