# Static configuration files
# This module contains:
# - Kubernetes pod configuration
# - Docker compose configuration
# - Cache configuration settings

{ pkgs }:

let
  # Multi-container pod configuration
  podConfig = pkgs.writeTextFile {
    name = "nanna-coder-pod.yaml";
    text = ''
      apiVersion: v1
      kind: Pod
      metadata:
        name: nanna-coder-pod
      spec:
        containers:
        - name: harness
          image: nanna-coder-harness:latest
          ports:
          - containerPort: 8080
          env:
          - name: OLLAMA_URL
            value: "http://localhost:11434"
          - name: RUST_LOG
            value: "info"
        - name: ollama
          image: nanna-coder-ollama:latest
          ports:
          - containerPort: 11434
          volumeMounts:
          - name: ollama-data
            mountPath: /root/.ollama
        volumes:
        - name: ollama-data
          emptyDir: {}
    '';
  };

  # Podman compose file for easier orchestration
  composeConfig = pkgs.writeTextFile {
    name = "docker-compose.yml";
    text = ''
      version: '3.8'

      services:
        ollama:
          image: nanna-coder-ollama:latest
          ports:
            - "11434:11434"
          volumes:
            - ollama_data:/root/.ollama
          environment:
            - OLLAMA_HOST=0.0.0.0
          healthcheck:
            test: ["CMD", "curl", "-f", "http://localhost:11434/api/tags"]
            interval: 30s
            timeout: 10s
            retries: 3
            start_period: 40s

        harness:
          image: nanna-coder-harness:latest
          ports:
            - "8080:8080"
          environment:
            - OLLAMA_URL=http://ollama:11434
            - RUST_LOG=info
          depends_on:
            ollama:
              condition: service_healthy
          command: ["harness", "chat", "--model", "llama3.1:8b", "--tools"]

      volumes:
        ollama_data:
    '';
  };

  # Cache size management configuration
  cacheConfig = {
    maxTotalSize = "10GB"; # Maximum total cache size
    maxModelAge = "30days"; # Auto-cleanup models older than this
    evictionPolicy = "LRU"; # Least Recently Used eviction
    compressionEnabled = true;
  };

in
{
  inherit podConfig composeConfig cacheConfig;
}
