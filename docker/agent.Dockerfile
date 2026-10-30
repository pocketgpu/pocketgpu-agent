# The agent for Docker and rented GPUs (RunPod, Vast.ai). Built by the release workflow from the
# release binaries; run with the NVIDIA container toolkit so NVML is available:
#   docker run -d --gpus all -v pocketgpu:/data ghcr.io/<owner>/pocketgpu
# Then pair from inside: docker exec -it <container> pgpu pair
FROM debian:12-slim

ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home --shell /usr/sbin/nologin pgpu \
    && mkdir /data && chown pgpu:pgpu /data

COPY --chmod=755 dist/${TARGETARCH}/pocketgpu /usr/local/bin/pocketgpu
RUN ln -s pocketgpu /usr/local/bin/pgpu

# State (key, paired phones, history) lives in /data so it survives new images.
ENV PGPU_DATA_DIR=/data \
    NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=utility
VOLUME /data
USER pgpu
ENTRYPOINT ["pocketgpu"]
CMD ["agent"]
