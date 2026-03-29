# Harness Control Flow

```mermaid
---
config:
  theme: redux-dark
  layout: dagre
---
flowchart TD
    A(["Application State 1"]) --> n6["Entity Enrichment"]
    n10(["User Prompt"]) --> n4["Plan Entity Modification"]
    B{"Task Complete?"} --> C["Yes"] & D["No"]
    D --> n1["Entity Modification Decision"]
    n1 --> n3["Query Entities (RAG)"] & n4
    n4 --> n7["Perform Entity Modification"]
    C --> n9(["Application State 2"])
    n3 --> n1
    n7 --> n11["Update Entities"]
    n11 --> B
    n6 --> n4
    n6@{ shape: rect}
    n4@{ shape: rect}
    n1@{ shape: diam}
    n3@{ shape: rect}
    n7@{ shape: rect}
    n11@{ shape: rect}
     A:::Rose
     A:::Aqua
     n10:::Aqua
     n9:::Aqua
    classDef Rose stroke-width:1px, stroke-dasharray:none, stroke:#FF5978, fill:#FFDFE5, color:#8E2236
    classDef Aqua stroke-width:1px, stroke-dasharray:none, stroke:#46EDC8, fill:#DEFFF8, color:#378E7A
```

# Container Topology

```mermaid
---
config:
  theme: redux-dark
  layout: elk
---
flowchart TD
    B(["Harness Container"]) -- Modifies --> C(["Dev Container"])
    B -- Queries --> n1(["Model"])
    C -- Can compile binary for --> n2(["Sandbox"])
    n2 -- Can be promoted to --> n3(["Release"])
```

# Tests and Evals

```mermaid
---
config:
  theme: redux-dark
---
flowchart TD
    subgraph UT["Unit Tests"]
        u1["Data types & serialization"]
        u2["Config parsing"]
        u3["Entity CRUD"]
        u4["Tool execution"]
        u5["Prompt construction"]
    end

    subgraph IT["Integration Tests"]
        i1["Container lifecycle"]
        i2["Agent loop (mock model)"]
        i3["MCP protocol"]
        i4["Security & provenance (shell)"]
        i5["Onboarding E2E"]
    end

    subgraph EV["Evals"]
        e1["Happy-path task cases"]
        e2["Decision-making quality"]
        e3["RAG accuracy"]
        e4["Model judge scoring"]
    end

    subgraph UT_IT["Unit ∩ Integration"]
        ui1["Agent state transitions"]
        ui2["Entity store operations"]
    end

    subgraph UT_EV["Unit ∩ Evals"]
        ue1["Eval case deserialization"]
        ue2["Eval report generation"]
    end

    subgraph IT_EV["Integration ∩ Evals"]
        ie1["End-to-end task completion"]
        ie2["Containerised agent runs"]
    end

    subgraph ALL["Unit ∩ Integration ∩ Evals"]
        a1["Agent decision loop"]
    end
```
