//! Agent architecture implementation
//!
//! This module implements the main agent control loop following ARCHITECTURE.md:
//!
//! 1. Application State 1 → **Entity Enrichment**
//! 2. Entity Enrichment → **Plan Entity Modification** ← User Prompt
//! 3. Plan Entity Modification → **Perform Entity Modification**
//! 4. Perform Entity Modification → **Update Entities**
//! 5. Update Entities → **Task Complete?**
//! 6. If Yes → Application State 2 (completed)
//! 7. If No → **Entity Modification Decision**
//! 8. Decision → **Query Entities (RAG)** → back to Decision
//! 9. Decision → **Plan Entity Modification** (loop)