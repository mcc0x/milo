import { defineRailway, github, project, service } from "railway/iac";

export default defineRailway(() => {
  const site = service("site", {
    source: github("mcc0x/milo", { branch: "main", checkSuites: false, rootDirectory: "/site" }),
    build: { buildEnvironment: "V3", builder: "RAILPACK", watchPatterns: ["/site/**"] },
    healthcheck: "/",
    replicas: { "us-west2": 1 },
    domains: ["usemilo.net"],
  });

  return project("Milo", {
    variables: { managed: false },
    resources: [site],
  });
});
