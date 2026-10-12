module.exports = {
  projects: {
    // Download the GraphQL schema from a local server. Mnemos IDE: upstream also had a
    // project for Warp's staging server, which this schema describes; Mnemos runs no
    // GraphQL server of its own, so only a local server you run yourself is listed.
    local: {
      schema: {
        'http://localhost:8080/graphql/v2': {
          loader: './api/client-schema.ts'
        }
      },
      extensions: {
        codegen: {
          generates: {
            'api/schema.graphql': {
              plugins: ['schema-ast']
            }
          }
        }
      }
    },
  }
};
