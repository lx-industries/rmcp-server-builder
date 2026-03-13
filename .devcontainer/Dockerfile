# syntax=docker/dockerfile:1

# -- Base image (reused from CI) -----------------------------------------------
# renovate: datasource=docker depName=rust
FROM rust:1.94.0@sha256:7e322aa1b876cbb977e0df46812af6c4e8be2efbfb2ce3712c28a93ba2968726

# -- Environment (set early so tool installers use the right paths) ------------
ENV HOME=/tmp/home
ENV PATH="${HOME}/.local/bin:${PATH}"

# NPM supply-chain hardening — Claude Code runs npm install for MCP servers.
ENV NPM_CONFIG_IGNORE_SCRIPTS=true
ENV NPM_CONFIG_MINIMUM_RELEASE_AGE=1440

# Forge CLI config directory
ENV GLAB_CONFIG_DIR=/tmp/glab-config

# -- System packages -----------------------------------------------------------
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg openssh-client \
        iptables ipset iproute2 dnsutils \
    && rm -rf /var/lib/apt/lists/*

# -- gosu (privilege drop for firewall mode + IDE UID inference) ---------------
# renovate: datasource=github-releases depName=tianon/gosu
ARG GOSU_VERSION="1.19"
RUN arch="$(dpkg --print-architecture)" \
    && curl -fsSL "https://github.com/tianon/gosu/releases/download/${GOSU_VERSION}/gosu-${arch}" -o /usr/local/bin/gosu \
    && chmod +x /usr/local/bin/gosu \
    && gosu --version

# -- GitLab CLI (glab) --------------------------------------------------------
# renovate: datasource=gitlab-releases depName=gitlab-org/cli registryUrl=https://gitlab.com
ARG GLAB_VERSION="1.88.0"
RUN arch="$(dpkg --print-architecture)" \
    && curl -fsSL "https://gitlab.com/gitlab-org/cli/-/releases/v${GLAB_VERSION}/downloads/glab_${GLAB_VERSION}_linux_${arch}.deb" -o /tmp/glab.deb \
    && dpkg -i /tmp/glab.deb \
    && rm /tmp/glab.deb \
    && glab --version \
    && rm -rf /tmp/glab-config \
    && mkdir -m 1777 /tmp/glab-config

# -- Claude Code (native binary, auto-updates) --------------------------------
# HOME is set above so the installer places the binary at $HOME/.local/bin/claude
RUN curl -fsSL https://claude.ai/install.sh | bash \
    && claude --version

# -- Git and SSH configuration ------------------------------------------------
RUN git config --system safe.directory '*'

RUN mkdir -p /etc/ssh \
    && ssh-keyscan -t ecdsa,rsa,ed25519 gitlab.com github.com >> /etc/ssh/ssh_known_hosts 2>/dev/null

# -- Firewall files ------------------------------------------------------------
COPY firewall.sh /usr/local/bin/firewall.sh
RUN chmod +x /usr/local/bin/firewall.sh

COPY firewall-allowlist.txt /usr/local/share/firewall-allowlist.txt

RUN printf '#!/bin/bash\ncat /tmp/firewall-allowed-ips.txt 2>/dev/null || echo "Firewall not active"\n' \
        > /usr/local/bin/firewall-list \
    && chmod +x /usr/local/bin/firewall-list

# -- Non-root user (for remoteUser + updateRemoteUserUID) ----------------------
RUN groupadd --gid 1000 dev \
 && useradd --uid 1000 --gid 1000 --no-create-home --home-dir /tmp/home --shell /bin/bash dev

# -- Permissions ---------------------------------------------------------------
RUN chmod -R 1777 /tmp/home

# Allow entrypoint to add passwd/group entries for arbitrary UIDs
RUN chmod 0666 /etc/passwd

# No WORKDIR — project mounted at host-native absolute path
# for git worktree compatibility (.git file contains absolute paths)

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["bash"]
