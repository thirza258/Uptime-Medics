# auth.md

## Overview
Authentication and registration guide for agents using Uptime Medics.

## Discovery
- OAuth Protected Resource Metadata: https://uptime.nevatal.id/.well-known/oauth-protected-resource
- OAuth 2.0 Authorization Server Metadata: https://uptime.nevatal.id/.well-known/oauth-authorization-server
- OpenID Connect Discovery: https://uptime.nevatal.id/.well-known/openid-configuration
- API Catalog: https://uptime.nevatal.id/.well-known/api-catalog
- Agent Card: https://uptime.nevatal.id/.well-known/agent-card.json

## Registration
Public content can be read without credentials. Agents that need write
access register at https://uptime.nevatal.id/api/agent/register and receive bearer credentials.
