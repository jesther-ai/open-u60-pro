import type { Metadata } from "next";
import { Callout } from "@/components/ui/Callout";
import { CodeBlock } from "@/components/ui/CodeBlock";

export const metadata: Metadata = {
  title: "Troubleshooting",
  description: "Common issues and solutions for zte-agent.",
};

export default function TroubleshootingPage() {
  return (
    <div className="space-y-8">
      <div>
        <h1 className="mb-2 font-display text-2xl font-bold">
          Troubleshooting
        </h1>
        <p className="text-text-dim">
          Common issues and how to resolve them.
        </p>
      </div>

      <section>
        <h2 className="mb-3 text-lg font-semibold">
          Login Failed / Wrong Password
        </h2>
        <p className="mb-3 text-sm text-text-dim">
          The &quot;Router Password&quot; field expects the admin password for your
          router&apos;s web interface (typically accessed at 192.168.0.1). This
          is not the WiFi password.
        </p>
        <Callout type="info">
          If you&apos;ve never changed it, try the default password printed on the
          router&apos;s label.
        </Callout>
      </section>

      <section>
        <h2 className="mb-3 text-lg font-semibold">
          Connection Timeout
        </h2>
        <p className="mb-3 text-sm text-text-dim">
          Ensure you are connected to the router&apos;s WiFi or Ethernet.
          The agent communicates directly with the router at the gateway
          IP (default: 192.168.0.1).
        </p>
        <p className="text-sm text-text-dim">
          If you&apos;ve changed the router&apos;s LAN IP, update the
          IP address in the companion app or API calls accordingly.
        </p>
      </section>

      <section>
        <h2 className="mb-3 text-lg font-semibold">
          Agent Not Responding After Deploy
        </h2>
        <p className="mb-3 text-sm text-text-dim">
          If the agent isn&apos;t reachable at port 9090:
        </p>
        <ul className="list-inside list-disc space-y-1 text-sm text-text-dim">
          <li>Wait 5-10 seconds and try again</li>
          <li>
            Check if the agent is running via SSH:
          </li>
        </ul>
        <CodeBlock
          code={`ssh -p 2222 root@192.168.0.1 'ps | grep zte-agent'`}
          language="bash"
          className="mt-2"
        />
        <p className="mt-2 text-sm text-text-dim">
          If the process isn&apos;t running, check the boot script:
        </p>
        <CodeBlock
          code={`ssh -p 2222 root@192.168.0.1 'cat /data/local/tmp/start_zte_agent.sh'
ssh -p 2222 root@192.168.0.1 'sh /data/local/tmp/start_zte_agent.sh'`}
          language="bash"
          className="mt-2"
        />
      </section>

      <section>
        <h2 className="mb-3 text-lg font-semibold">
          CORS Errors
        </h2>
        <p className="text-sm text-text-dim">
          The zte-agent sends no CORS headers, and it refuses requests from
          other websites that change settings, so a page on another site
          cannot use the API from your browser. Use the native apps, or the
          agent&apos;s own web UI at http://192.168.0.1:9090/.
        </p>
      </section>
    </div>
  );
}
