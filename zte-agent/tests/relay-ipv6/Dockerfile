FROM debian:bookworm-slim
RUN apt-get update -qq \
    && apt-get install -y --no-install-recommends iproute2 iptables procps python3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
ENTRYPOINT []
CMD ["python3", "/qa/netns.py", "/plan.json"]
