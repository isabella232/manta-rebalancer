/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

@Library('jenkins-joylib@v1.0.4') _

pipeline {

    agent {
        label joyCommonLabels(image_ver: '19.2.0')
    }

    options {
        buildDiscarder(logRotator(numToKeepStr: '30'))
        timestamps()
    }

    parameters {
        string(
            name: 'AGENT_PREBUILT_AGENT_BRANCH',
            defaultValue: '',
            description: 'The branch to use for the agents ' +
                'that are included in this component.<br/>' +
                'With an empty value, the build will look for ' +
                'agents from the same branch name as the ' +
                'component, before falling back to "master".'
        )
    }

    stages {
        stage('check') {
            steps{
                sh('make check')
            }
        }
        stage('build image and upload') {
           steps {
               joyBuildImageAndUpload()
           }
        }
    }

    post {
        always {
            joyMattermostNotification(channel: 'jenkins')
            joyMattermostNotification(channel: 'rebalancer')
        }
    }
}
