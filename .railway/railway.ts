import { defineRailway, github, project, service, volume } from "railway/iac";

export default defineRailway((ctx) => {
  if (!ctx.isEnvironment("production")) throw new Error("This configuration manages Milo production only.");
  const site = service("site", {
    source: github("mcc0x/milo", { branch: "main", checkSuites: false, rootDirectory: "/site" }),
    build: { buildEnvironment: "V3", builder: "RAILPACK", watchPatterns: ["/site/**"] },
    healthcheck: "/",
    replicas: { "us-west2": 1 },
    domains: ["usemilo.net"],
  });

  const connect = service("connect", {
    source: github("mcc0x/milo", { branch: "main", checkSuites: true }),
    build: {
      builder: "DOCKERFILE",
      dockerfilePath: "hosted/Dockerfile",
      watchPatterns: [
        "/.dockerignore", "/hosted/Dockerfile", "/hosted/docker-entrypoint.sh",
        "/hosted/{bindings,edge,providers,result,server,service}.ts",
        "/dashboard/app/meadow/**", "/dashboard/public/favicon.png",
      ],
    },
    healthcheck: "/readyz",
    healthcheckTimeout: 90,
    deploy: { sleepApplication: false, overlapSeconds: 0, drainingSeconds: 15 },
    replicas: { "us-west2": 1 },
    // Register connect.usemilo.net after provisioning; Railway cannot create a custom domain from IaC.
    volumeMounts: { "/data": volume("telegram-bindings", { sizeMB: 512, region: "us-west2" }) },
  });

  return project("Milo", {
    variables: { managed: false },
    resources: [site, connect],
  });
});
